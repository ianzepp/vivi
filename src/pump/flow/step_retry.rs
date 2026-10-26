//! Step transient classification from machine evidence only: one pure,
//! typed verdict per finished step job, separating retryable transient
//! evidence, permanent environment faults and ordinary work results.
//!
//! The classifier reads the pool's outcome facts (`state` + `detail`), the
//! receipt's summary lines and the receipt's stderr and runner-log tail
//! sections ([`attempt_record::sections_tail`]). It never scans the hand's
//! final message for provider or checkout markers: markers in worker prose
//! are not machine evidence. The only thing it measures in the message is
//! emptiness — absent or whitespace-only reads the same, the receipt helper
//! trims — and only when the owner expects a model reply and the receipt
//! summary proves a clean agent exit. Lost and timeout eligibility is not
//! universal: the owning collector supplies it per job kind, so command
//! jobs and background cadence are never classified here.

use crate::pump::attempt_record;
use crate::pump::envfail;

/// Characters of evidence a verdict keeps before it truncates.
const EVIDENCE_CHARS: usize = 200;

/// Lowercased markers of authentication / configuration refusal. Matched
/// against the scan scope only, never the hand's message.
const AUTH_MARKERS: &[&str] = &[
    "invalid api key",
    "invalid_api_key",
    "incorrect api key",
    "unauthorized",
    " 401 ",
    " 403 ",
];

/// Lowercased markers of provider throttling: the HTTP 429 family.
const THROTTLED_MARKERS: &[&str] = &[
    "429",
    "rate limit",
    "rate_limit",
    "ratelimit",
    "too many requests",
];

/// Lowercased markers of provider overload.
const OVERLOAD_MARKERS: &[&str] = &["overloaded", "over capacity"];

/// Lowercased markers of a retryable provider / transport failure: the
/// gateway codes and connection faults `envfail` knows, without the auth
/// half it shares a rule with.
const PROVIDER_MARKERS: &[&str] = &[
    "connection error",
    "connection refused",
    "connection timed out",
    "connection reset by peer",
    "connection closed",
    " 502 ",
    " 503 ",
    " 504 ",
    "bad gateway",
    "service unavailable",
    "service temporarily",
    "gateway timeout",
    "internal server error",
];

/// The machine evidence one finished step job offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Evidence<'a> {
    /// The pool reconcile state: `finished`, `timeout` or `lost`.
    pub state: &'a str,
    /// The reconcile detail line.
    pub detail: &'a str,
    /// The full task receipt, summary lines and sections.
    pub receipt: &'a str,
}

/// What the owning collector supplies about one job kind's eligibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Eligibility {
    /// The owner expects this job to end in a model reply.
    pub model_reply: bool,
    /// A `lost` job of this kind is eligible for a retry.
    pub lost: bool,
    /// A `timeout` job of this kind is eligible for a retry.
    pub timeout: bool,
}

/// Retryable transient evidence: a later dispatch of the same step may do
/// better.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transient {
    /// `lost`, `timeout`, `runner-fetch`, `runner-checkout`, `throttled`,
    /// `overload`, `provider` or `empty-reply`.
    pub class: &'static str,
    /// One line of machine evidence, at most 200 characters.
    pub evidence: String,
}

/// A permanent environment fault: the same dispatch cannot do better, so
/// retrying it would only repeat the fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    /// `auth`, `missing-tool`, `target` or `workdir`.
    pub class: &'static str,
    /// One line of machine evidence, at most 200 characters.
    pub evidence: String,
}

/// The classifier's verdict for one finished step job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Retryable transient evidence of the named class.
    Retryable(Transient),
    /// A permanent environment fault of the named class.
    Permanent(Fault),
    /// An ordinary work result — success or a real unit failure. No retry
    /// evidence; the collector's usual path decides.
    Work,
}

/// Classify one finished step job. First rule hit wins: the pool's own
/// lost/timeout state (only when the owning collector made it eligible),
/// the runner's own push outcome, auth refusal, a missing tool, throttling,
/// overload, provider/transport failure, and finally an absent (or
/// whitespace-only) or literal-null model reply. Anything else is an
/// ordinary work result.
#[must_use]
pub fn classify(evidence: &Evidence<'_>, eligibility: Eligibility) -> Verdict {
    let scope = scan_scope(evidence);
    lost_or_timeout(evidence, eligibility)
        .or_else(|| runner_push(evidence))
        .or_else(|| marker_fault(&scope, AUTH_MARKERS, "auth"))
        .or_else(|| missing_tool(evidence, &scope))
        .or_else(|| marker_transient(&scope, THROTTLED_MARKERS, "throttled"))
        .or_else(|| marker_transient(&scope, OVERLOAD_MARKERS, "overload"))
        .or_else(|| marker_transient(&scope, PROVIDER_MARKERS, "provider"))
        .or_else(|| empty_reply(evidence, eligibility))
        .unwrap_or(Verdict::Work)
}

/// The pool's own lost/timeout state, retried only when the owning
/// collector made this job kind eligible. An ineligible lost or timeout
/// falls through untouched.
fn lost_or_timeout(evidence: &Evidence<'_>, eligibility: Eligibility) -> Option<Verdict> {
    let (eligible, class) = match evidence.state {
        "lost" => (eligibility.lost, "lost"),
        "timeout" => (eligibility.timeout, "timeout"),
        _ => return None,
    };
    if !eligible {
        return None;
    }
    let evidence_line = match capped(evidence.detail).is_empty() {
        true => format!("pool {class}"),
        false => capped(evidence.detail),
    };
    Some(Verdict::Retryable(Transient {
        class,
        evidence: evidence_line,
    }))
}

/// The runner's own push outcome: a `finished` job whose detail names one
/// of `envfail::HOST_PUSH_OUTCOMES`. Fetch and checkout failures are
/// runner-established hiccups worth a retry; a missing worktree or an
/// invalid target cannot succeed the same way twice.
fn runner_push(evidence: &Evidence<'_>) -> Option<Verdict> {
    if evidence.state != "finished" {
        return None;
    }
    let outcome = evidence.detail.split_whitespace().next()?;
    if !envfail::HOST_PUSH_OUTCOMES.contains(&outcome) {
        return None;
    }
    match outcome {
        "fetch_failed" => Some(retryable("runner-fetch", evidence.detail)),
        "checkout_failed" => Some(retryable("runner-checkout", evidence.detail)),
        "workdir_missing" => Some(Verdict::Permanent(Fault {
            class: "workdir",
            evidence: capped(evidence.detail),
        })),
        _ => Some(Verdict::Permanent(Fault {
            class: "target",
            evidence: capped(evidence.detail),
        })),
    }
}

/// A missing tool: the summary's agent exit 126/127, a scope line pinning
/// `(exit 126)` or `(exit 127)` on a command, or `command not found`.
fn missing_tool(evidence: &Evidence<'_>, scope: &str) -> Option<Verdict> {
    let exit = attempt_record::agent_exit(evidence.receipt);
    if exit.is_some_and(|code| envfail::missing_tool(code, "")) {
        return Some(Verdict::Permanent(Fault {
            class: "missing-tool",
            evidence: format!("agent exit {}", exit.unwrap_or_default()),
        }));
    }
    let line = scope.lines().find(|line| {
        line.contains("command not found")
            || line.contains("(exit 126)")
            || line.contains("(exit 127)")
    })?;
    Some(Verdict::Permanent(Fault {
        class: "missing-tool",
        evidence: capped(line),
    }))
}

/// An absent (empty or whitespace-only) or literal-null model reply — and
/// only that: the owner must expect a model reply and the receipt summary
/// must prove a clean agent exit 0, so an ordinary failure keeps
/// precedence over an accompanying blank reply. The receipt helper already
/// trims the message body, so whitespace-only reads as absent.
fn empty_reply(evidence: &Evidence<'_>, eligibility: Eligibility) -> Option<Verdict> {
    if !eligibility.model_reply || attempt_record::agent_exit(evidence.receipt) != Some(0) {
        return None;
    }
    let message = attempt_record::hand_message(evidence.receipt);
    let shape = if message.trim().is_empty() {
        "absent"
    } else if message.trim().eq_ignore_ascii_case("null") {
        "literal null"
    } else {
        return None;
    };
    Some(Verdict::Retryable(Transient {
        class: "empty-reply",
        evidence: format!("model reply {shape}"),
    }))
}

/// The first scope line carrying one of the lowercased `markers`, as a
/// retryable transient of `class`.
fn marker_transient(scope: &str, markers: &[&str], class: &'static str) -> Option<Verdict> {
    let line = marker_line(scope, markers)?;
    Some(retryable(class, line))
}

/// The first scope line carrying one of the lowercased `markers`, as a
/// permanent fault of `class`.
fn marker_fault(scope: &str, markers: &[&str], class: &'static str) -> Option<Verdict> {
    let line = marker_line(scope, markers)?;
    Some(Verdict::Permanent(Fault {
        class,
        evidence: capped(line),
    }))
}

/// The first scope line carrying one of the lowercased `markers`.
fn marker_line<'a>(scope: &'a str, markers: &[&str]) -> Option<&'a str> {
    scope.lines().find(|line| {
        markers
            .iter()
            .any(|marker| line.to_lowercase().contains(marker))
    })
}

/// A retryable transient with the first line of `evidence`.
fn retryable(class: &'static str, evidence: &str) -> Verdict {
    Verdict::Retryable(Transient {
        class,
        evidence: capped(evidence),
    })
}

/// The lines marker rules may read: the outcome detail plus the receipt's
/// stderr and runner-log tails — never the hand's final message.
fn scan_scope(evidence: &Evidence<'_>) -> String {
    let tail = attempt_record::sections_tail(evidence.receipt);
    if tail.is_empty() {
        evidence.detail.to_owned()
    } else {
        format!("{}\n{tail}", evidence.detail)
    }
}

/// The first line of `text`, trimmed, capped at [`EVIDENCE_CHARS`] with the
/// ellipsis `envfail` uses when it truncates.
fn capped(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    let mut out: String = line.chars().take(EVIDENCE_CHARS).collect();
    if out.len() != line.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
#[path = "step_retry_test.rs"]
mod tests;
