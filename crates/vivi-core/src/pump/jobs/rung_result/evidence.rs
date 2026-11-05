//! Bounded, fail-closed comparison of rung outcomes.
use serde::{Deserialize, Serialize};

use super::{RungResult, failure_text};

/// Maximum output admitted as comparison evidence.
pub const MAX_EVIDENCE_BYTES: usize = 256 * 1024;

/// A serializable snapshot suitable for retaining rung comparison evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// Command exit code, if reported by the runner.
    pub exit: Option<i32>,
    /// Canonical Cargo failing IDs from all available receipt/stderr output.
    pub failing: Vec<String>,
    /// Whether runner capture summaries prove both relevant streams complete.
    pub complete: bool,
    /// Whether the output contains recognized Cargo test failure evidence.
    pub recognized: bool,
    /// Whether selected output fits the evidence bound.
    pub bounded: bool,
    /// Canonical compiler/clippy diagnostic signatures.
    #[serde(default)]
    pub diagnostics: Vec<String>,
    /// Whether every diagnostic-looking block was parsed completely.
    #[serde(default)]
    pub diagnostics_complete: bool,
}

/// Result of comparing a candidate against a prior rung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Comparison {
    /// Candidate passed, or has no new failing IDs relative to baseline.
    Acceptable,
    /// Candidate introduced at least one canonical failing test ID.
    NewRed,
    /// Evidence cannot safely justify the subset decision.
    Unavailable,
}

/// Extract full selected output before display-tail truncation is applied.
#[must_use]
pub fn evidence(result: &RungResult, receipt: &str) -> Evidence {
    let full = failure_text(receipt);
    let byte_bounded = full.len() <= MAX_EVIDENCE_BYTES;
    let complete = capture_complete(receipt, "receipt") && capture_complete(receipt, "stderr");
    let parsed_diagnostics = diagnostics::parse(&full);
    let diagnostics_complete = parsed_diagnostics.is_some();
    let diagnostics = parsed_diagnostics.unwrap_or_default();
    let recognized = full.lines().any(|line| {
        line.starts_with("test ") && line.ends_with(" ... FAILED")
            || line.starts_with("{\"type\":\"test\"")
    }) || !diagnostics.is_empty();
    Evidence {
        exit: result.exit,
        failing: result.failing.iter().cloned().collect(),
        complete,
        recognized,
        bounded: byte_bounded,
        diagnostics,
        diagnostics_complete,
    }
}

/// Compare results using fail-closed requirements for every nonzero candidate.
#[must_use]
pub fn compare(baseline: &RungResult, candidate: &RungResult) -> Comparison {
    if candidate.exit == Some(0) {
        return Comparison::Acceptable;
    }
    let (Some(_), Some(_)) = (baseline.exit, candidate.exit) else {
        return Comparison::Unavailable;
    };
    let baseline_evidence = evidence_for(baseline);
    let candidate_evidence = evidence_for(candidate);
    if !valid(&baseline_evidence) || !valid(&candidate_evidence) {
        return Comparison::Unavailable;
    }
    let baseline_ids: std::collections::BTreeSet<_> = baseline.failing.iter().collect();
    let baseline_diagnostics: std::collections::BTreeSet<_> =
        baseline_evidence.diagnostics.iter().collect();
    if candidate.failing.iter().all(|id| baseline_ids.contains(id))
        && candidate_evidence
            .diagnostics
            .iter()
            .all(|item| baseline_diagnostics.contains(item))
    {
        Comparison::Acceptable
    } else {
        Comparison::NewRed
    }
}

fn evidence_for(result: &RungResult) -> Evidence {
    result.evidence.clone().unwrap_or(Evidence {
        exit: result.exit,
        failing: result.failing.iter().cloned().collect(),
        complete: false,
        recognized: !result.failing.is_empty(),
        bounded: false,
        diagnostics: Vec::new(),
        diagnostics_complete: false,
    })
}

fn valid(evidence: &Evidence) -> bool {
    evidence.exit.is_some_and(|exit| exit != 0)
        && evidence.complete
        && evidence.recognized
        && evidence.bounded
        && evidence.diagnostics_complete
        && (!evidence.failing.is_empty() || !evidence.diagnostics.is_empty())
}

fn capture_complete(text: &str, name: &str) -> bool {
    let prefix = format!("runner capture {name}: ");
    text.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .is_some_and(|value| value.ends_with(" bytes complete"))
}

#[path = "diagnostics.rs"]
pub mod diagnostics;

#[cfg(test)]
#[path = "evidence_test.rs"]
mod tests;
