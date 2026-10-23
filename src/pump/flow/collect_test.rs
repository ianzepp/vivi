//! Tests for collecting role-step outcomes from envelopes and runner receipts.

use std::fs;

use tempfile::{TempDir, tempdir};

use super::{Collected, collect};
use crate::pump::config::PumpConfig;
use crate::pump::envelope::EnvelopeError;
use crate::pump::flow::def::NodeClass;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::index::StepJobRow;
use crate::pump::flow::mail::MailWriter;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob};

const REF: &str = "refs/heads/need/n1";
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

struct Fixture {
    _dir: TempDir,
    ctx: Ctx,
    mail: FakeMail,
    task: String,
}

fn fixture() -> Fixture {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let mut mail = FakeMail::new("pump");
    let task = mail
        .send_step("pump", "pump", "plan", "pump-step: n1 r1 plan", &[])
        .unwrap();
    Fixture {
        _dir: dir,
        ctx,
        mail,
        task,
    }
}

fn job(id: &str, status: JobStatus, receipt: &str) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new(id, "planner", "planner", "s", "b"));
    job.status = status;
    job.outcome = Some(JobOutcome {
        state: "finished".to_owned(),
        detail: String::new(),
        receipt: receipt.to_owned(),
        at: 0,
    });
    job
}

fn step(f: &Fixture, job: &str, attempt: u32) -> StepJobRow {
    StepJobRow {
        job: job.to_owned(),
        run: 1,
        node: "plan".to_owned(),
        task: f.task.clone(),
        attempt,
    }
}

fn exceptions(f: &Fixture) -> String {
    fs::read_to_string(f.ctx.home.join("exceptions.txt")).unwrap_or_default()
}

fn run(f: &mut Fixture, job: &PoolJob, attempt: u32, class: &NodeClass) -> Collected {
    let row = step(f, &job.id, attempt);
    collect(&f.ctx, &mut f.mail, job, &row, class, REF)
}

const PLANNED: &str = "+++\nanswer = \"planned\"\n+++\nThe plan.";
const BROKEN: &str = "+++\nanswer = \"planned\"\nproof = 'x'\n+++\nThe plan.";

#[test]
fn a_valid_envelope_is_the_outcome() {
    let mut f = fixture();
    let got = run(
        &mut f,
        &job("j1", JobStatus::Finished, PLANNED),
        1,
        &NodeClass::Planner,
    );
    let Collected::Outcome(word, payload) = got else {
        panic!("expected an outcome, got {got:?}");
    };
    assert_eq!(word, "planned");
    assert_eq!(payload["front"]["answer"], "planned");
    assert_eq!(payload["body"], "The plan.");
}

#[test]
fn a_broken_envelope_gives_a_correction_not_a_retry() {
    let mut f = fixture();
    let got = run(
        &mut f,
        &job("j1", JobStatus::Finished, BROKEN),
        1,
        &NodeClass::Planner,
    );
    assert!(matches!(got, Collected::Correct { .. }), "{got:?}");
}

#[test]
fn a_missing_envelope_gives_a_correction() {
    let mut f = fixture();
    let got = run(
        &mut f,
        &job("j1", JobStatus::Finished, "ANSWER: planned"),
        2,
        &NodeClass::Planner,
    );
    assert_eq!(
        got,
        Collected::Correct {
            error: EnvelopeError::NoOpening
        }
    );
}

#[test]
fn the_corrected_output_is_an_outcome_with_the_attempt_unchanged() {
    let mut f = fixture();
    let got = run(
        &mut f,
        &job("j1-c1", JobStatus::Finished, PLANNED),
        1,
        &NodeClass::Planner,
    );
    assert!(matches!(got, Collected::Outcome(word, _) if word == "planned"));
    assert!(exceptions(&f).is_empty());
}

#[test]
fn a_third_broken_output_is_failed_with_an_alarm() {
    let mut f = fixture();
    let second = run(
        &mut f,
        &job("j1-c1", JobStatus::Finished, BROKEN),
        1,
        &NodeClass::Planner,
    );
    assert!(matches!(second, Collected::Correct { .. }));
    assert!(exceptions(&f).is_empty());
    let third = run(
        &mut f,
        &job("j1-c2", JobStatus::Finished, BROKEN),
        1,
        &NodeClass::Planner,
    );
    assert_eq!(third, Collected::Failed);
    let text = exceptions(&f);
    assert!(text.contains("flow-envelope-failed"), "{text}");
    assert!(text.contains("j1-c2"), "{text}");
}

#[test]
fn timeout_and_lost_retry_then_fail() {
    for status in [JobStatus::Timeout, JobStatus::Lost] {
        let mut f = fixture();
        let j = job("j1", status, PLANNED);
        assert_eq!(run(&mut f, &j, 1, &NodeClass::Planner), Collected::Retry);
        assert_eq!(run(&mut f, &j, 2, &NodeClass::Planner), Collected::Failed);
        assert!(exceptions(&f).is_empty());
    }
}

#[test]
fn a_class_without_a_role_is_failed() {
    let mut f = fixture();
    let got = run(
        &mut f,
        &job("j1", JobStatus::Finished, PLANNED),
        1,
        &NodeClass::RungNeed,
    );
    assert_eq!(got, Collected::Failed);
}

fn merge_receipt(publish: Option<&str>, message: &str) -> String {
    let mut text = "agent exit 0 in 3s\n".to_owned();
    if let Some(line) = publish {
        text.push_str(line);
        text.push('\n');
    }
    text.push_str("\n--- receipt\n");
    text.push_str(message);
    text
}

fn published(reference: &str) -> String {
    format!("runner publish: pushed {reference} {SHA} {SHA}")
}

const SAYS_MERGED: &str = "+++\nanswer = \"merged\"\n+++\nResolved.";
const SAYS_CONFLICT: &str = "+++\nanswer = \"conflict\"\nconflicts = ['a.rs']\n+++\nStuck.";

#[test]
fn a_merge_with_a_publish_receipt_is_merged_with_its_sha() {
    let mut f = fixture();
    let receipt = merge_receipt(Some(&published(REF)), SAYS_MERGED);
    let got = run(
        &mut f,
        &job("m1", JobStatus::Finished, &receipt),
        1,
        &NodeClass::Merge,
    );
    let Collected::Outcome(word, payload) = got else {
        panic!("expected an outcome, got {got:?}");
    };
    assert_eq!(word, "merged");
    assert_eq!(payload["sha"], SHA);
    assert!(!exceptions(&f).contains("flow-envelope-disagrees"));
}

#[test]
fn a_merge_receipt_beats_an_envelope_that_says_conflict() {
    let mut f = fixture();
    let receipt = merge_receipt(Some(&published(REF)), SAYS_CONFLICT);
    let got = run(
        &mut f,
        &job("m1", JobStatus::Finished, &receipt),
        1,
        &NodeClass::Merge,
    );
    assert!(
        matches!(&got, Collected::Outcome(word, _) if word == "merged"),
        "{got:?}"
    );
    assert!(exceptions(&f).contains("flow-envelope-disagrees"));
}

#[test]
fn a_clean_exit_with_no_publish_is_a_conflict() {
    let mut f = fixture();
    let receipt = merge_receipt(None, SAYS_CONFLICT);
    let got = run(
        &mut f,
        &job("m1", JobStatus::Finished, &receipt),
        1,
        &NodeClass::Merge,
    );
    assert!(
        matches!(&got, Collected::Outcome(word, _) if word == "conflict"),
        "{got:?}"
    );
    assert!(!exceptions(&f).contains("flow-envelope-disagrees"));
}

#[test]
fn a_merge_with_no_decisive_receipt_is_verified_against_git() {
    let mut f = fixture();
    for status in [JobStatus::Timeout, JobStatus::Lost] {
        let receipt = merge_receipt(Some(&published(REF)), SAYS_MERGED);
        let got = run(&mut f, &job("m1", status, &receipt), 1, &NodeClass::Merge);
        assert_eq!(got, Collected::Verify);
    }
    let mut no_outcome = job("m1", JobStatus::Finished, "");
    no_outcome.outcome = None;
    assert_eq!(
        run(&mut f, &no_outcome, 1, &NodeClass::Merge),
        Collected::Verify
    );
    let wrong_ref = merge_receipt(Some(&published("refs/heads/other")), SAYS_MERGED);
    let got = run(
        &mut f,
        &job("m1", JobStatus::Finished, &wrong_ref),
        1,
        &NodeClass::Merge,
    );
    assert_eq!(got, Collected::Verify);
    let crashed = "agent exit 1 in 3s\n\n--- receipt\nboom";
    let got = run(
        &mut f,
        &job("m1", JobStatus::Finished, crashed),
        1,
        &NodeClass::Merge,
    );
    assert_eq!(got, Collected::Verify);
}

#[test]
fn a_broken_merge_envelope_with_a_decisive_receipt_gets_no_correction() {
    let mut f = fixture();
    let receipt = merge_receipt(Some(&published(REF)), "no envelope here");
    let got = run(
        &mut f,
        &job("m1", JobStatus::Finished, &receipt),
        1,
        &NodeClass::Merge,
    );
    assert!(
        matches!(&got, Collected::Outcome(word, _) if word == "merged"),
        "{got:?}"
    );
    assert!(exceptions(&f).contains("flow-envelope-failed"));
    let replied = f.mail.calls.iter().any(|call| {
        matches!(call, Call::Reply { handle, body }
            if *handle == f.task && body.starts_with("pump-envelope-error:"))
    });
    assert!(replied, "{:?}", f.mail.calls);
}
