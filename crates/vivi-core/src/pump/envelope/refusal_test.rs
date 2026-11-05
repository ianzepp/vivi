//! Tests for evidence-aware envelope refusal diagnoses.

use super::describe;
use crate::pump::envelope::EnvelopeError;
use crate::pump::jobs::{JobOutcome, JobSpec, PoolJob};

fn job(id: &str, read_only: bool) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new(id, "planner", "planner", "s", "b"));
    job.read_only = read_only;
    job
}

fn with_receipt(id: &str, read_only: bool, receipt: &str) -> PoolJob {
    let mut job = job(id, read_only);
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::new(),
        receipt: receipt.to_owned(),
        at: 0,
    });
    job
}

fn discarded(total: u64, path: Option<&str>) -> String {
    let mut receipt = format!("result: read_only_discarded {total}\n");
    if let Some(path) = path {
        receipt.push_str("discarded paths (1 of 1):\n  ");
        receipt.push_str(path);
        receipt.push('\n');
    }
    receipt.push_str("\n--- receipt\nreviewed\n");
    receipt
}

#[test]
fn no_opening_leads_with_the_fact_and_the_discarded_evidence() {
    let job = with_receipt("j1", true, &discarded(3, Some("src/a.rs")));
    let got = describe(&job, &EnvelopeError::NoOpening);
    assert!(got.starts_with("reply has no envelope;"), "{got}");
    assert!(got.contains("the read-only job changed 3 files"), "{got}");
    assert!(got.contains("(first: src/a.rs)"), "{got}");
    let changed = got.find("changed 3 files").unwrap();
    let advice = got.find("`+++`").unwrap();
    assert!(
        changed < advice,
        "count and path precede fence advice: {got}"
    );
}

#[test]
fn no_opening_without_evidence_invents_no_change_or_filename() {
    let got = describe(&job("j1", true), &EnvelopeError::NoOpening);
    assert!(got.starts_with("reply has no envelope"), "{got}");
    assert!(!got.contains("changed"), "{got}");
    assert!(!got.contains("(first:"), "{got}");
}

#[test]
fn a_missing_sampled_path_still_reports_the_exact_total() {
    let job = with_receipt("j1", true, &discarded(7, None));
    let got = describe(&job, &EnvelopeError::NoOpening);
    assert!(got.contains("the read-only job changed 7 files"), "{got}");
    assert!(!got.contains("(first:"), "{got}");
}

#[test]
fn an_unchanged_read_only_job_reports_no_changes() {
    let job = with_receipt("j1", true, &discarded(0, None));
    let got = describe(&job, &EnvelopeError::NoOpening);
    assert!(got.contains("reply has no envelope"), "{got}");
    assert!(!got.contains("changed"), "{got}");
}

#[test]
fn a_single_discarded_file_is_singular() {
    let job = with_receipt("j1", true, &discarded(1, Some("a.rs")));
    let got = describe(&job, &EnvelopeError::NoOpening);
    assert!(got.contains("changed 1 file (first: a.rs)"), "{got}");
}

#[test]
fn malformed_envelopes_keep_their_typed_explanation_and_add_evidence() {
    let job = with_receipt("j1", true, &discarded(2, Some("dir/b c.rs")));
    let error = EnvelopeError::Unclosed;
    let got = describe(&job, &error);
    assert!(got.starts_with(&error.to_string()), "{got}");
    assert!(got.contains("the read-only job changed 2 files"), "{got}");
    assert!(got.contains("(first: dir/b c.rs)"), "{got}");
}

#[test]
fn malformed_envelopes_without_evidence_keep_the_typed_explanation() {
    let error = EnvelopeError::Toml {
        line: 4,
        message: String::from("bad key"),
    };
    let got = describe(&job("j1", true), &error);
    assert_eq!(got, error.to_string());
}

#[test]
fn a_writable_jobs_receipt_is_not_discarded_read_only_evidence() {
    let job = with_receipt("j1", false, &discarded(5, Some("published.rs")));
    let got = describe(&job, &EnvelopeError::NoOpening);
    assert!(got.contains("reply has no envelope"), "{got}");
    assert!(!got.contains("changed"), "{got}");
    assert!(!got.contains("published.rs"), "{got}");
}
