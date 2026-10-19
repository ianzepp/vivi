//! Classification of environment failures from dispatch evidence: a host
//! or harness problem (missing tool, locked worktree, dead endpoint, failed
//! checkout) is not a unit failure, and the pump must treat it differently.

use super::attempt_record;

/// How far back, in seconds, host-failure evidence counts toward a pause.
pub const HOST_FAIL_WINDOW: u64 = 300;

/// Host failures inside the window after which a pool pauses.
pub const HOST_PAUSE_AFTER: usize = 2;

/// Push outcomes that name a host problem, not a unit problem.
pub const HOST_PUSH_OUTCOMES: [&str; 10] = [
    "checkout_failed",
    "fetch_failed",
    "workdir_missing",
    "target_prepare_failed",
    "target_requirement_failed",
    "target_seed_failed",
    "target_key_invalid",
    "target_cache_invalid",
    "target_source_mismatch",
    "target_worktree_missing",
];

const AGENT_EXIT: &str = "agent exit ";

/// A host or harness failure read out of dispatch evidence.
#[derive(Debug, PartialEq, Eq)]
pub struct HostFailure {
    /// Which rule classified it: `push_outcome`, `agent_exit`,
    /// `facts_exit`, `shell_marker`, `endpoint`, or `provider`.
    pub class: String,
    /// The command the host could not run, when a rule found one.
    pub command: Option<String>,
    /// One-line summary: the class and the first 200 chars of evidence.
    pub detail: String,
}

/// Classify dispatch evidence as a host failure; first rule hit wins: the
/// push outcome, the agent's exit code, the receipt facts, shell markers
/// in the detail or the receipt's tail sections, endpoint trouble or
/// provider/transport failure in that same scope. `None` means the
/// evidence looks like a unit failure.
#[must_use]
pub fn classify(why: &str, detail: &str, receipt: &str) -> Option<HostFailure> {
    push_outcome(why, detail)
        .or_else(|| agent_exit_failure(receipt))
        .or_else(|| fact_exit(receipt))
        .or_else(|| shell_markers(detail, receipt))
        .or_else(|| endpoint(detail, receipt))
        .or_else(|| provider(detail, receipt))
}

/// Whether an exit code or a text line shows a tool the host lacks.
#[must_use]
pub fn missing_tool(code: i32, text: &str) -> bool {
    code == 126 || code == 127 || text.contains("command not found")
}

/// Whether the check was killed by Vivi's timeout wrapper.
///
/// The process runner uses exit 124 and appends `[timeout after Ns]`. Accept
/// either signal so callers also handle receipts that preserve only one part
/// of the evidence.
#[must_use]
pub fn timed_out(code: i32, text: &str) -> bool {
    code == 124 || (code != 0 && text.lines().any(timeout_marker))
}

/// The duration from the timeout wrapper's exact marker line, when present.
#[must_use]
pub fn timeout_seconds(text: &str) -> Option<u64> {
    text.lines().rev().find_map(|line| {
        let seconds = line
            .trim()
            .strip_prefix("[timeout after ")?
            .strip_suffix("s]")?;
        if seconds.is_empty() || !seconds.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        match seconds.parse() {
            Ok(seconds) => Some(seconds),
            Err(_) => None,
        }
    })
}

/// Match only the exact marker appended by the host timeout wrapper.
fn timeout_marker(line: &str) -> bool {
    let line = line.trim();
    line.strip_prefix("[timeout after ")
        .and_then(|seconds| seconds.strip_suffix("s]"))
        .is_some_and(|seconds| {
            !seconds.is_empty() && seconds.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// The push outcome names a host problem and the pool finished the job.
fn push_outcome(why: &str, detail: &str) -> Option<HostFailure> {
    let outcome = detail.split_whitespace().next()?;
    if !why.starts_with("finished") || !HOST_PUSH_OUTCOMES.contains(&outcome) {
        return None;
    }
    Some(failure("push_outcome", None, detail))
}

/// The agent itself exited with the missing-tool codes.
fn agent_exit_failure(receipt: &str) -> Option<HostFailure> {
    let exit = attempt_record::agent_exit(receipt)?;
    if exit != 126 && exit != 127 {
        return None;
    }
    let evidence = receipt
        .lines()
        .find(|line| line.contains(AGENT_EXIT))
        .map_or_else(|| format!("{AGENT_EXIT}{exit}"), str::to_owned);
    Some(failure("agent_exit", None, &evidence))
}

/// A facts line pins a command exit: `$ pi --version  (exit 127)`.
fn fact_exit(receipt: &str) -> Option<HostFailure> {
    let line = receipt.lines().find(|line| fact_command(line).is_some())?;
    let cmd = fact_command(line)?;
    let command = cmd.split_whitespace().next().map(str::to_owned);
    Some(failure("facts_exit", command, line))
}

/// The command of a `$ <cmd>  (exit 126|127)` facts line, when it is one.
fn fact_command(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("$ ")?.strip_suffix(')')?;
    let (cmd, status) = rest.split_once("  (")?;
    (status == "exit 126" || status == "exit 127").then_some(cmd)
}

/// Shell markers, scanned only over the detail and the receipt's tail
/// sections, never the agent's own message section — so a unit about
/// exit-127 bugs cannot trip it.
fn shell_markers(detail: &str, receipt: &str) -> Option<HostFailure> {
    for line in scan_scope(detail, receipt).lines() {
        if line.contains("command not found") {
            let token = line.split(':').next().unwrap_or_default();
            let command = token.trim().rsplit(' ').next().map(str::to_owned);
            return Some(failure("shell_marker", command, line));
        }
        if (line.contains("index.lock") && line.contains("File exists"))
            || line.contains("Another git process seems to be running")
        {
            return Some(failure("shell_marker", None, line));
        }
    }
    None
}

/// Endpoint trouble: auth rejects or the gateway is down.
fn endpoint(detail: &str, receipt: &str) -> Option<HostFailure> {
    const CODES: [&str; 5] = [" 401 ", " 403 ", " 502 ", " 503 ", " 504 "];
    const MARKERS: [&str; 7] = [
        "Bad Gateway",
        "Service Unavailable",
        "Service Temporarily",
        "invalid api key",
        "invalid_api_key",
        "Incorrect API key",
        "Unauthorized",
    ];
    for line in scan_scope(detail, receipt).lines() {
        let hit = CODES.iter().any(|code| line.contains(code))
            || MARKERS.iter().any(|marker| line.contains(marker));
        if hit {
            return Some(failure("endpoint", None, line));
        }
    }
    None
}

/// Provider/transport trouble: the agent ended against a dead or dropped
/// upstream connection, not a unit bug.
fn provider(detail: &str, receipt: &str) -> Option<HostFailure> {
    const MARKERS: [&str; 5] = [
        "Connection error",
        "Connection refused",
        "Connection timed out",
        "connection reset by peer",
        "Connection closed",
    ];
    for line in scan_scope(detail, receipt).lines() {
        if MARKERS.iter().any(|marker| line.contains(marker)) {
            return Some(failure("provider", None, line));
        }
    }
    None
}

/// The lines host-marker rules may read: the detail plus the receipt's
/// stderr and runner-log tails.
fn scan_scope(detail: &str, receipt: &str) -> String {
    let tail = attempt_record::sections_tail(receipt);
    if tail.is_empty() {
        detail.to_owned()
    } else {
        format!("{detail}\n{tail}")
    }
}

/// Build the failure record: the class, the command, and a one-line
/// summary naming the class and the first 200 chars of the evidence.
fn failure(class: &str, command: Option<String>, evidence: &str) -> HostFailure {
    let line = evidence.lines().next().unwrap_or_default();
    let mut text: String = line.chars().take(200).collect();
    if text.len() != line.len() {
        text.push('…');
    }
    HostFailure {
        class: class.to_owned(),
        command,
        detail: format!("{class}: {text}"),
    }
}
