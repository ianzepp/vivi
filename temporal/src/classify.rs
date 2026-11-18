//! Turn a command's exit status and output into the README's failure
//! classes, and a class into the typed failure the workflows act on.
//!
//! | Class | Workflow policy |
//! |---|---|
//! | Provider (429, 5xx, quota) | non-retryable failure carrying Retry-After; the workflow waits on a timer and runs the step again without spending an attempt |
//! | Infra (host, ssh, slot, git transport) | retryable; after the attempts the step moves to another host, then parks |
//! | Format (the agent's reply was refused) | retried like infra |
//! | Work (a red check) | not an error: returned as a value |

use std::time::Duration;

use temporalio_sdk::ApplicationFailure;
use temporalio_sdk::activities::ActivityError;

use crate::defs::{FORMAT_FAILURE, INFRA_FAILURE, PROVIDER_FAILURE};

/// The longest Retry-After taken from provider text.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(3_600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureClass {
    Provider { retry_after: Option<Duration> },
    Infra,
    Format,
    Work,
}

const PROVIDER_TEXT: &[&str] = &[
    "too many requests",
    "rate limit",
    "rate-limit",
    "ratelimit",
    "rate_limit",
    "insufficient_quota",
    "quota exceeded",
    "exceeded your current quota",
    "usage limit",
    "overloaded",
    "service unavailable",
    "bad gateway",
    "gateway timeout",
];

const INFRA_TEXT: &[&str] = &[
    "ssh:",
    "connection refused",
    "connection reset",
    "connection closed",
    "could not resolve",
    "name or service not known",
    "nodename nor servname",
    "network is unreachable",
    "no route to host",
    "broken pipe",
    "host key verification failed",
    "operation timed out",
    "no free slot",
    "slots are in use",
    "has no slots",
    "slot is busy",
    "lost the slot",
    "could not read from remote repository",
];

const FORMAT_TEXT: &[&str] = &["rejected:", "reply was refused", "no verdict line"];

/// An agent or LLM step that failed. Provider and format signatures win;
/// anything else is the host's fault (infra), never bad work.
pub fn classify_agent(text: &str) -> FailureClass {
    let lower = text.to_ascii_lowercase();
    if is_provider(&lower) {
        return FailureClass::Provider {
            retry_after: retry_after(&lower),
        };
    }
    if FORMAT_TEXT.iter().any(|p| lower.contains(p)) {
        return FailureClass::Format;
    }
    FailureClass::Infra
}

/// A machine check that exited. `None` (killed at its time limit) and the
/// shell's "cannot run" codes are infra; any other failure is red work.
pub fn classify_check(rc: Option<i32>, text: &str) -> FailureClass {
    match rc {
        None | Some(126) | Some(127) => FailureClass::Infra,
        Some(_)
            if INFRA_TEXT
                .iter()
                .any(|p| text.to_ascii_lowercase().contains(p)) =>
        {
            FailureClass::Infra
        }
        Some(_) => FailureClass::Work,
    }
}

/// A plumbing command (git, ssh, vivi) that failed: provider when the text
/// says so, else infra.
pub fn classify_plumbing(text: &str) -> FailureClass {
    let lower = text.to_ascii_lowercase();
    if is_provider(&lower) {
        return FailureClass::Provider {
            retry_after: retry_after(&lower),
        };
    }
    FailureClass::Infra
}

fn is_provider(lower: &str) -> bool {
    PROVIDER_TEXT.iter().any(|p| lower.contains(p))
        || has_code(lower, "429")
        || ["http 5", "status 5", "status: 5", "status code 5"]
            .iter()
            .any(|p| lower.contains(p))
}

/// `code` as a whole token: "HTTP 429" yes, "a1429f" no.
fn has_code(lower: &str, code: &str) -> bool {
    let bytes = lower.as_bytes();
    lower.match_indices(code).any(|(i, _)| {
        let before = i.checked_sub(1).map(|j| bytes[j]);
        let after = bytes.get(i + code.len()).copied();
        !before.is_some_and(|b| b.is_ascii_alphanumeric())
            && !after.is_some_and(|b| b.is_ascii_alphanumeric())
    })
}

/// A Retry-After named in provider text: `retry-after: 30`, `Retry after
/// 20s`, `try again in 1500ms`.
pub fn retry_after(lower: &str) -> Option<Duration> {
    let keys = ["retry-after", "retry_after", "retry after", "try again in"];
    keys.iter().find_map(|key| {
        let at = lower.find(key)? + key.len();
        let rest = lower[at..].trim_start_matches([' ', ':', '=', '"']);
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let n: u64 = digits.parse().ok()?;
        let unit = rest[digits.len()..].trim_start();
        let wait = if unit.starts_with("ms") {
            Duration::from_millis(n)
        } else if unit.starts_with("min") {
            Duration::from_secs(n.saturating_mul(60))
        } else {
            Duration::from_secs(n)
        };
        Some(wait.min(MAX_RETRY_AFTER))
    })
}

/// The typed activity failure for a class. `Work` never reaches here in
/// the activities (it is a value), so it maps to a plain non-retryable
/// failure if it ever does.
pub fn to_activity_error(class: &FailureClass, message: String) -> ActivityError {
    let failure = match class {
        FailureClass::Provider { retry_after } => {
            let builder = ApplicationFailure::builder(message)
                .type_name(PROVIDER_FAILURE.to_string())
                .non_retryable(true);
            match retry_after {
                Some(wait) => builder.next_retry_delay(*wait).build(),
                None => builder.build(),
            }
        }
        FailureClass::Infra => ApplicationFailure::builder(message)
            .type_name(INFRA_FAILURE.to_string())
            .build(),
        FailureClass::Format => ApplicationFailure::builder(message)
            .type_name(FORMAT_FAILURE.to_string())
            .build(),
        FailureClass::Work => ApplicationFailure::builder(message)
            .type_name("work".to_string())
            .non_retryable(true)
            .build(),
    };
    ActivityError::application(failure)
}

#[cfg(test)]
#[path = "classify_test.rs"]
mod tests;
