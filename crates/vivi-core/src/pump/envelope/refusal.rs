//! Human-readable envelope refusal diagnoses for operators and corrections.
//!
//! The diagnosis names what the envelope did wrong and, when the machine
//! receipt holds it, the discarded read-only change evidence that goes with
//! the refusal. Only [`JobOutcome::read_only_discarded`] supplies that
//! evidence; this module never reads receipt text itself.

use super::EnvelopeError;
use super::validation::diagnostic;
use crate::pump::jobs::PoolJob;

/// Describe why `job`'s reply was refused, with any trustworthy discarded
/// change evidence in front of optional fence advice.
#[must_use]
pub fn describe(job: &PoolJob, error: &EnvelopeError) -> String {
    let evidence = evidence(job);
    let mut text = match error {
        EnvelopeError::NoOpening => missing_envelope(evidence.as_ref()),
        _ => default_explanation(error, evidence.as_ref()),
    };
    if text.is_empty() {
        text.push_str("the reply was refused");
    }
    text
}

/// Discarded evidence the machine receipt records for a read-only job.
struct Evidence {
    total: u64,
    first: Option<String>,
}

fn evidence(job: &PoolJob) -> Option<Evidence> {
    if !job.read_only {
        return None;
    }
    let (total, first) = job.outcome.as_ref()?.read_only_discarded()?;
    (total > 0).then_some(Evidence { total, first })
}

fn missing_envelope(evidence: Option<&Evidence>) -> String {
    let mut text = String::from("reply has no envelope");
    if let Some(evidence) = evidence {
        text.push_str("; ");
        text.push_str(&changed(evidence));
    }
    text.push_str("; ");
    text.push_str(&diagnostic(&EnvelopeError::NoOpening));
    text
}

fn default_explanation(error: &EnvelopeError, evidence: Option<&Evidence>) -> String {
    let mut text = diagnostic(error);
    if let Some(evidence) = evidence {
        text.push_str("; ");
        text.push_str(&changed(evidence));
    }
    text
}

fn changed(evidence: &Evidence) -> String {
    let noun = if evidence.total == 1 { "file" } else { "files" };
    match &evidence.first {
        Some(path) => format!(
            "the read-only job changed {} {noun} (first: {path})",
            evidence.total
        ),
        None => format!("the read-only job changed {} {noun}", evidence.total),
    }
}

#[cfg(test)]
#[path = "refusal_test.rs"]
mod tests;
