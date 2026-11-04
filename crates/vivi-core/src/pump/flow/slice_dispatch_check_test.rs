//! A built hand envelope with a failing `[[check]]` is a refused attempt.

use std::fs;

use tempfile::tempdir;

use super::{AttemptResult, HAND_REPORTED_FAILURE, classify};
use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob};
use vivi_error::VivariumError;

fn receipt(message: &str) -> String {
    format!(
        "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
         push: pushed vivi/unit/t0001-a1 abcdef0123\n\
         agent exit 0 in 12s, job 20s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n\n\n--- runner log tail\nlog line"
    )
}

fn envelope(checks: &[(&str, &str)]) -> String {
    let mut text = String::from("+++\nanswer = \"built\"\nchange = [\"src/a.rs\"]\n");
    for (cmd, result) in checks {
        text.push_str(&format!(
            "\n[[check]]\ncmd = \"{cmd}\"\nresult = \"{result}\"\nseconds = 3\n"
        ));
    }
    text.push_str("+++\nDone.");
    text
}

fn job(message: &str) -> PoolJob {
    let mut spec = JobSpec::new("slice-t0001-a1", "hand", "hand", "s", "b");
    spec.task = Some("t0001".into());
    spec.branch = Some("vivi/unit/t0001-a1".into());
    let mut job = PoolJob::from(spec);
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: receipt(message),
        at: 0,
    });
    job
}

fn run(checks: &[(&str, &str)]) -> Result<(AttemptResult, String), VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let got = classify(&ctx, &job(&envelope(checks))).expect("terminal job");
    let log = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap_or_default();
    Ok((got, log))
}

#[test]
fn a_failing_check_makes_the_built_attempt_rejected_with_its_pushed_branch()
-> Result<(), VivariumError> {
    let (got, log) = run(&[("cargo fmt --check", "pass"), ("cargo test x", "fail")])?;
    let AttemptResult::Rejected { reason, branch } = got else {
        panic!("expected rejected, got {got:?}");
    };
    assert!(
        reason.contains("reports failing checks: cargo test x"),
        "{reason}"
    );
    assert_eq!(branch.as_deref(), Some("vivi/unit/t0001-a1"));
    assert!(log.contains(HAND_REPORTED_FAILURE), "{log}");
    assert!(log.contains("cargo test x"), "{log}");
    assert!(!log.contains("cargo fmt --check"), "{log}");
    assert!(log.contains("slice-t0001-a1"), "{log}");
    Ok(())
}

#[test]
fn passing_and_skipped_checks_stay_built() -> Result<(), VivariumError> {
    let (got, log) = run(&[
        ("cargo check", "pass"),
        ("cargo test --test hygiene", "skipped"),
    ])?;
    assert!(matches!(got, AttemptResult::Built { .. }), "{got:?}");
    assert!(!log.contains(HAND_REPORTED_FAILURE), "{log}");
    Ok(())
}
