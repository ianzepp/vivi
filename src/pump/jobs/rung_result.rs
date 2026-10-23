//! Reading a rung job's result from its pool receipt.

use std::collections::BTreeSet;

use crate::pump::attempt_record::agent_exit;
use crate::pump::jobs::{JobStatus, PoolJob};

/// Most lines of receipt output a result keeps.
const TAIL_LINES: usize = 60;
/// Most bytes of receipt output a result keeps.
const TAIL_BYTES: usize = 4096;

/// What a finished rung command reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RungResult {
    /// The command exited 0.
    pub pass: bool,
    /// The command's exit code; `None` when the job never reported one.
    pub exit: Option<i32>,
    /// The end of the receipt section followed by the stderr tail section of
    /// the posted reply, capped by lines and bytes. A reply with no sections
    /// is tailed whole.
    pub tail: String,
    /// Canonical ids of the tests the output named as failing, read from
    /// cargo's own output and never from model text: `<binary>::<test path>`,
    /// or just `<test path>` when the binary is unknown. The binary is the
    /// deps file stem without its trailing `-<hex hash>`, with `-` replaced by
    /// `_`, so every spelling of one crate gives one id.
    ///
    /// Consumer contract (N6 main-rung red detection, which compares failing
    /// sets between runs, and N7 repair-flow failing-test identity): compare
    /// sets of these ids only. An empty set with `pass == false` means the
    /// output named no failing test: a build error, a timeout, a lost job, or
    /// output cut by the 256 KiB head capture of agent.out
    /// (`src/pool/collect.rs`). An empty set is never green and never "all
    /// fixed".
    pub failing: BTreeSet<String>,
}

/// Read a rung job's result. A timed-out or lost job, or a receipt with no
/// agent exit, does not pass and carries no exit code.
#[must_use]
pub fn parse(job: &PoolJob) -> RungResult {
    let receipt = job.outcome.as_ref().map_or("", |o| o.receipt.as_str());
    let text = failure_text(receipt);
    let tail = tail_of(&text);
    let exit = match job.status {
        JobStatus::Finished => agent_exit(receipt),
        _ => None,
    };
    RungResult {
        pass: exit == Some(0),
        exit,
        tail,
        failing: failing_ids(&text),
    }
}

/// The exception kind the flow raises for an environment failure: the worker
/// lacks a tool or its host setup failed, so the job says nothing about the
/// code under test. Raised once per subject, never retried.
pub const ENV_FAILURE: &str = "flow-env-failure";

/// Longest evidence line an environment failure keeps, in characters.
const EVIDENCE_CHARS: usize = 200;

/// What kind of environment failure a job hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvKind {
    /// A command the job needs is not installed on the worker.
    ToolMissing,
    /// Git or the checkout, fetch or target setup failed on the host.
    HostSetup,
}

/// A job that failed because of its worker, read from machine facts only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvFailure {
    /// Which kind of environment failure.
    pub kind: EnvKind,
    /// The missing command, when the facts name one.
    pub tool: Option<String>,
    /// One line of evidence, at most 200 characters.
    pub evidence: String,
}

/// Read an environment failure from a job's machine facts: the agent exit
/// code, the runner facts (`envfail::classify`) and shell markers in the
/// receipt's stderr tail. Model text is never read. `None` when the job has
/// no outcome, succeeded, or failed for another reason. The first rule that
/// matches wins.
#[must_use]
pub fn env_failure(job: &PoolJob) -> Option<EnvFailure> {
    let outcome = job.outcome.as_ref()?;
    if outcome.agent_success() == Some(true) {
        return None;
    }
    let stderr = stderr_marker(&outcome.receipt);
    let tool_of = |from_facts: Option<&str>| {
        from_facts
            .map(str::to_owned)
            .or_else(|| stderr.as_ref().and_then(|(tool, _)| tool.clone()))
    };
    if let Some(code @ (126 | 127)) = parse(job).exit {
        let evidence = match &stderr {
            Some((_, line)) => line.clone(),
            None => format!("agent exit {code}"),
        };
        return Some(env(EnvKind::ToolMissing, tool_of(None), &evidence));
    }
    if let Some(host) = outcome.host_failure() {
        match host.class.as_str() {
            "agent_exit" | "facts_exit" => {
                let tool = tool_of(host.command.as_deref());
                return Some(env(EnvKind::ToolMissing, tool, &host.detail));
            }
            "shell_marker" => {
                let missing = host.detail.contains("command not found");
                let kind = if missing {
                    EnvKind::ToolMissing
                } else {
                    EnvKind::HostSetup
                };
                let tool = if missing { tool_of(None) } else { None };
                return Some(env(kind, tool, &host.detail));
            }
            "push_outcome" => return Some(env(EnvKind::HostSetup, None, &host.detail)),
            _ => {}
        }
    }
    stderr.map(|(tool, line)| env(EnvKind::ToolMissing, tool, &line))
}

fn env(kind: EnvKind, tool: Option<String>, evidence: &str) -> EnvFailure {
    let line = evidence.lines().next().unwrap_or_default().trim();
    EnvFailure {
        kind,
        tool,
        evidence: line.chars().take(EVIDENCE_CHARS).collect(),
    }
}

/// The first missing-tool line in the receipt's stderr tail section: the tool
/// (when the line names it) and the line. Reads that section only, never the
/// agent's own message.
fn stderr_marker(receipt: &str) -> Option<(Option<String>, String)> {
    let mut sections = receipt.split("\n\n--- ");
    sections.next();
    for section in sections {
        let (title, body) = section.split_once('\n').unwrap_or((section, ""));
        if title.trim() != "stderr tail" {
            continue;
        }
        for line in body.lines().map(str::trim) {
            if let Some(tool) = missing_tool_of(line) {
                return Some((Some(tool), line.to_owned()));
            }
            if line.contains("command not found") {
                return Some((None, line.to_owned()));
            }
        }
    }
    None
}

/// The tool of `<shell>: <tool>: command not found` or the dash form
/// `sh: <n>: <tool>: not found`.
fn missing_tool_of(line: &str) -> Option<String> {
    let tool = if let Some(head) = line.strip_suffix(": command not found") {
        head.rsplit(": ").next()?
    } else {
        let head = line.strip_prefix("sh: ")?.strip_suffix(": not found")?;
        let (number, tool) = head.split_once(": ")?;
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        tool
    };
    let tool = tool.trim();
    (!tool.is_empty() && !tool.contains(char::is_whitespace)).then(|| tool.to_owned())
}

/// The binary a cargo `Running` or `Doc-tests` line names, canonical; the
/// outer `None` for any other line.
fn binary_line(line: &str) -> Option<Option<String>> {
    let line = line.trim();
    if let Some(name) = line.strip_prefix("Doc-tests ") {
        return Some(Some(canonical(name.trim())));
    }
    if !line.starts_with("Running ") {
        return None;
    }
    let inner = line.strip_suffix(')').and_then(|l| l.rsplit_once('('));
    let stem = inner.and_then(|(_, path)| path.rsplit('/').next());
    Some(stem.filter(|s| !s.is_empty()).map(canonical))
}

/// A deps file stem without its trailing `-<hex hash>`, `-` replaced by `_`.
fn canonical(stem: &str) -> String {
    let bare = match stem.rsplit_once('-') {
        Some((head, hash)) if hash.len() >= 8 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => {
            head
        }
        _ => stem,
    };
    bare.replace('-', "_")
}

/// The binary named by a cargo `to rerun pass` footer, canonical.
fn footer_binary(line: &str) -> Option<String> {
    let command = line.split('`').nth(1)?;
    let words: Vec<&str> = command.split_whitespace().collect();
    let after = |flags: &[&str]| {
        words
            .iter()
            .position(|w| flags.contains(w))
            .and_then(|i| words.get(i + 1))
            .map(|w| canonical(w))
    };
    after(&["--test", "--bin"]).or_else(|| after(&["-p", "--package"]))
}

/// The test name of a libtest `... FAILED` line or a JSON failed event.
fn failed_line(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("test ")
        && let Some(name) = rest.strip_suffix(" ... FAILED")
    {
        return Some(name.trim().to_owned());
    }
    if !line.starts_with('{') {
        return None;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return None;
    };
    if value["type"] != "test" || value["event"] != "failed" {
        return None;
    }
    value["name"].as_str().map(str::to_owned)
}

/// Where the parser is within a libtest `failures:` list.
#[derive(Clone, Copy, PartialEq)]
enum Block {
    None,
    Header,
    Names,
}

/// Canonical ids of the failing tests named in cargo output.
fn failing_ids(text: &str) -> BTreeSet<String> {
    let mut binary: Option<String> = None;
    let mut found: BTreeSet<(Option<String>, String)> = BTreeSet::new();
    let mut footers: BTreeSet<Option<String>> = BTreeSet::new();
    let mut state = Block::None;
    for line in text.lines() {
        if let Some(named) = binary_line(line) {
            binary = named;
            state = Block::None;
            continue;
        }
        if line.contains("to rerun pass `") {
            footers.insert(footer_binary(line));
        }
        if let Some(name) = failed_line(line) {
            found.insert((binary.clone(), name));
            continue;
        }
        let listed = state != Block::None && line.starts_with("    ") && !line.trim().is_empty();
        if listed {
            found.insert((binary.clone(), line.trim().to_owned()));
        }
        state = if line == "failures:" {
            Block::Header
        } else if listed {
            Block::Names
        } else if state == Block::Header && line.is_empty() {
            Block::Header
        } else {
            Block::None
        };
    }
    let lone = if footers.len() == 1 {
        footers.into_iter().next().flatten()
    } else {
        None
    };
    found
        .into_iter()
        .map(|(own, name)| match own.or_else(|| lone.clone()) {
            Some(bin) => format!("{bin}::{name}"),
            None => name,
        })
        .collect()
}

/// Section headings `pool::record::collected_text` writes.
const HEADINGS: [&str; 4] = [
    "--- facts",
    "--- receipt",
    "--- stderr tail",
    "--- runner log tail",
];

/// The receipt and stderr sections of a posted reply, joined; the whole text
/// when it has no section headings.
fn failure_text(text: &str) -> String {
    if !text.lines().any(|line| HEADINGS.contains(&line)) {
        return text.to_owned();
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut keeping = false;
    for line in text.lines() {
        if HEADINGS.contains(&line) {
            keeping = line == "--- receipt" || line == "--- stderr tail";
        } else if keeping {
            kept.push(line);
        }
    }
    kept.join("\n")
}

fn tail_of(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let kept = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
    if kept.len() <= TAIL_BYTES {
        return kept;
    }
    let mut start = kept.len() - TAIL_BYTES;
    while !kept.is_char_boundary(start) {
        start += 1;
    }
    kept[start..].to_owned()
}

#[cfg(test)]
#[path = "rung_result_test.rs"]
mod tests;
