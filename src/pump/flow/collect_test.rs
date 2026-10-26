//! Tests for collecting role-step outcomes from envelopes and runner receipts.

use std::fs;

use tempfile::{TempDir, tempdir};

use super::{BUDGET_UNRAISABLE, Collected, collect};
use crate::letter::fixture::step_body;
use crate::pool::dispatch::BUDGET_RANGE;
use crate::pump::config::PumpConfig;
use crate::pump::envelope::EnvelopeError;
use crate::pump::flow::budget_retry;
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
        .send_step("pump", "pump", "plan", &step_body("n1", 1, "plan", ""), &[])
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

/// The pool summary head of a job the runner killed at its time budget.
const KILL_SUMMARY: &str = "unit a1\nagent exit 124 after 600s\npool: local";

/// A `status` job killed at its time budget, stored with `budget`.
fn killed(id: &str, status: JobStatus, budget: u64) -> PoolJob {
    let mut killed = job(id, status, KILL_SUMMARY);
    killed.budget = budget;
    killed
}

#[test]
fn lost_and_nonthinking_timeouts_keep_the_attempt_limit() {
    // A lost job keeps the unchanged-budget attempt rule whatever its
    // receipt claims, and so does every nonthinking class: neither ever
    // spends the budget retry, which is raised-marker business only.
    for (status, class) in [
        (JobStatus::Lost, NodeClass::Planner),
        (JobStatus::Timeout, NodeClass::Repairer),
    ] {
        let mut f = fixture();
        let j = job("j1", status, PLANNED);
        assert_eq!(run(&mut f, &j, 1, &class), Collected::Retry);
        assert_eq!(run(&mut f, &j, 2, &class), Collected::Failed);
        assert!(exceptions(&f).is_empty());
    }
}

#[test]
fn a_first_budget_kill_raises_the_stored_budget() {
    // Representation one: the pool's terminal timeout status.
    let mut f = fixture();
    let j = killed("j1", JobStatus::Timeout, 600);
    assert_eq!(
        run(&mut f, &j, 1, &NodeClass::Planner),
        Collected::BudgetRetry { budget: 1_200 }
    );
    assert!(exceptions(&f).is_empty());
    // Representation two: a finished job whose bounded pool summary reports
    // agent exit 124; the raise is the stored budget doubled.
    let mut f = fixture();
    let j = killed("j1", JobStatus::Finished, 600);
    assert_eq!(
        run(&mut f, &j, 1, &NodeClass::Planner),
        Collected::BudgetRetry { budget: 1_200 }
    );
    assert!(exceptions(&f).is_empty());
}

#[test]
fn budget_kill_beats_a_partial_valid_envelope() {
    let mut f = fixture();
    let mut j = killed("j1", JobStatus::Finished, 600);
    // The kill summary is followed by a receipt section carrying a valid
    // `planned` envelope: the machine evidence decides before any parse,
    // so the envelope is never read.
    j.outcome = Some(JobOutcome {
        state: "finished".to_owned(),
        detail: String::new(),
        receipt: format!("{KILL_SUMMARY}\n\n--- receipt\n{PLANNED}"),
        at: 0,
    });
    assert_eq!(
        run(&mut f, &j, 1, &NodeClass::Planner),
        Collected::BudgetRetry { budget: 1_200 }
    );
    assert!(exceptions(&f).is_empty());
}

#[test]
fn budget_kill_is_machine_evidence_only() {
    // Exit 124 in the stderr tail is prose, not pool evidence: the summary
    // says exit 0, so the valid envelope alone decides and it stands.
    let mut f = fixture();
    let prose = format!(
        "pool result j1: finished\nagent exit 0 in 8s\n\n--- receipt\n{PLANNED}\
         \n\n--- stderr tail\nchild command output: agent exit 124 after 30s"
    );
    let got = run(
        &mut f,
        &job("j1", JobStatus::Finished, &prose),
        1,
        &NodeClass::Planner,
    );
    assert!(
        matches!(&got, Collected::Outcome(word, _) if word == "planned"),
        "{got:?}"
    );
    assert!(exceptions(&f).is_empty());
    // A lost job is never a kill, even with a kill-shaped receipt: the
    // ordinary attempt rule holds.
    let mut f = fixture();
    let j = killed("j1", JobStatus::Lost, 600);
    assert_eq!(run(&mut f, &j, 1, &NodeClass::Planner), Collected::Retry);
    assert!(exceptions(&f).is_empty());
}

#[test]
fn a_spent_budget_kill_fails_without_another_raise() {
    // The durable marker spends the one retry at any attempt count: the
    // killed job answers failed for both kill representations.
    for status in [JobStatus::Timeout, JobStatus::Finished] {
        let mut f = fixture();
        let mut j = killed("j1", status, 600);
        budget_retry::mark_retry(&mut j.payload);
        assert_eq!(run(&mut f, &j, 1, &NodeClass::Planner), Collected::Failed);
        assert_eq!(run(&mut f, &j, 2, &NodeClass::Planner), Collected::Failed);
        assert!(exceptions(&f).is_empty());
    }
}

#[test]
fn an_unraisable_budget_kill_fails_with_a_recorded_reason() {
    let mut f = fixture();
    // Budget zero is outside the pool range: no strict raise exists, so the
    // kill fails through the existing route with a recorded reason naming
    // node, job and budget instead of queueing an invalid rerun.
    let j = killed("j1", JobStatus::Timeout, 0);
    assert_eq!(run(&mut f, &j, 1, &NodeClass::Planner), Collected::Failed);
    let text = exceptions(&f);
    assert!(text.contains(BUDGET_UNRAISABLE), "{text}");
    assert!(text.contains("plan job j1"), "{text}");
    assert!(text.contains("budget 0"), "{text}");
    // A budget already at the pool ceiling cannot strictly rise either.
    let mut f = fixture();
    let j = killed("j1", JobStatus::Timeout, *BUDGET_RANGE.end());
    assert_eq!(run(&mut f, &j, 1, &NodeClass::Planner), Collected::Failed);
    let text = exceptions(&f);
    assert!(text.contains(BUDGET_UNRAISABLE), "{text}");
    assert!(text.contains("budget 43200"), "{text}");
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

fn cto_run(f: &mut Fixture, id: &str, run_number: u32, front: &str) -> Collected {
    let receipt = format!("+++\n{front}+++\nThe view.");
    let job = job(id, JobStatus::Finished, &receipt);
    let mut row = step(f, &job.id, 1);
    row.run = run_number;
    collect(&f.ctx, &mut f.mail, &job, &row, &NodeClass::Cto, REF)
}

fn correct_error(got: &Collected) -> String {
    let Collected::Correct { error } = got else {
        panic!("expected a correction, got {got:?}");
    };
    error.to_string()
}

#[test]
fn run_three_rework_without_new_fact_is_corrected() {
    let mut f = fixture();
    let got = cto_run(&mut f, "j1", 3, "answer = \"rework\"\nreason = 'r'\n");
    assert!(correct_error(&got).contains("new_fact"), "{got:?}");
}

#[test]
fn run_three_rework_with_blank_new_fact_is_corrected() {
    let mut f = fixture();
    let front = "answer = \"rework\"\nreason = 'r'\nnew_fact = '  '\n";
    let got = cto_run(&mut f, "j1", 3, front);
    assert!(correct_error(&got).contains("new_fact"), "{got:?}");
}

#[test]
fn run_three_rework_with_new_fact_is_the_outcome() {
    let mut f = fixture();
    let front = "answer = \"rework\"\nreason = 'r'\nnew_fact = 'X'\n";
    let got = cto_run(&mut f, "j1", 3, front);
    assert!(matches!(got, Collected::Outcome(word, _) if word == "rework"));
}

#[test]
fn run_three_lock_needs_no_new_fact() {
    let mut f = fixture();
    let got = cto_run(&mut f, "j1", 3, "answer = \"lock\"\nreason = 'r'\n");
    assert!(matches!(got, Collected::Outcome(word, _) if word == "lock"));
}

#[test]
fn run_two_plan_needs_no_new_fact() {
    let mut f = fixture();
    let got = cto_run(&mut f, "j1", 2, "answer = \"plan\"\nreason = 'r'\n");
    assert!(matches!(got, Collected::Outcome(word, _) if word == "plan"));
}

#[test]
fn run_three_without_new_fact_after_two_corrections_is_failed() {
    let mut f = fixture();
    let got = cto_run(&mut f, "j1-c2", 3, "answer = \"slice\"\nreason = 'r'\n");
    assert_eq!(got, Collected::Failed);
}
