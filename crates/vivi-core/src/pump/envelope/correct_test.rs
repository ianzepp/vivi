//! Tests for the envelope correction job.

use std::collections::BTreeMap;

use tempfile::tempdir;

use super::{KIND, base_id, correction_n, enqueue, resumable, spec};
use crate::pump::envelope::EnvelopeError;
use crate::pump::flow::index::StepJobRow;
use crate::pump::jobs::{JobOutcome, JobSpec, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use vivi_mail::letter::roles::{Planner, RoleKind, parse_role_reply};

#[derive(Default)]
struct Store {
    jobs: BTreeMap<String, PoolJob>,
}

impl JobStore for Store {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.jobs
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.jobs
    }
}

fn row(job: &str, attempt: u32) -> StepJobRow {
    StepJobRow {
        job: job.to_owned(),
        run: 2,
        node: "plan".to_owned(),
        task: "t1".to_owned(),
        attempt,
    }
}

fn original(id: &str, message: &str) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new(id, "planner", "planner", "s", "b"));
    job.target = Some("vivi".to_owned());
    job.from_ref = Some("main".to_owned());
    job.extra_targets = vec!["vivi-server".to_owned()];
    job.with = vec!["need/x".to_owned()];
    job.builtin_role = Some("planner".to_owned());
    job.model = Some("m1".to_owned());
    job.outcome = Some(JobOutcome {
        state: "finished".to_owned(),
        detail: String::new(),
        receipt: message.to_owned(),
        at: 0,
    });
    job
}

#[test]
fn the_spec_names_the_step_and_asks_for_the_envelope_only() {
    let job = original("j1", "the plan, no fences");
    let error = EnvelopeError::NoOpening;
    let got = spec(
        "n1",
        &row("j1", 1),
        "planner",
        RoleKind::Planner,
        &job,
        &error,
        1,
        45,
    );
    assert_eq!(got.id, "j1-c1");
    assert_eq!(got.kind, KIND);
    assert_eq!(got.role, "planner");
    assert!(got.read_only);
    assert_eq!(got.budget, 45);
    assert_eq!(got.target.as_deref(), Some("vivi"));
    assert!(
        got.payload
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
    );
    assert!(
        got.body
            .starts_with("Correction for pump step n1 r2 plan.\n"),
        "{}",
        got.body
    );
    assert!(got.body.contains(&error.to_string()));
    assert!(got.body.contains("Rewrite only the envelope"));
    assert!(got.body.contains("keep every fact"));
    assert!(got.body.contains("planner: answer"));
    assert!(got.body.contains("the plan, no fences"));
    let original = got.body.find("The reply to correct:").unwrap();
    let footer = got
        .body
        .find("The envelope rules and this role's fields:")
        .unwrap();
    assert!(original < footer, "faulty reply must precede final footer");
    let (prefix, example) = got
        .body
        .rsplit_once("Example (illustrative only):\n")
        .unwrap();
    assert!(prefix.contains("The envelope rules and this role's fields:"));
    assert!(
        got.body.ends_with("+++"),
        "the shared footer example is final"
    );
    assert!(
        got.body
            .contains("Rewrite only the envelope; do not redo the work; keep every fact.")
    );
    let parsed = parse_role_reply::<Planner>(example).unwrap();
    assert_eq!(
        parsed.front.answer,
        vivi_mail::letter::roles::PlannerAnswer::Delivered
    );
}

#[test]
fn the_correction_carries_the_originals_dispatch_fields() {
    let job = original("j1", "x");
    let got = spec(
        "n1",
        &row("j1", 1),
        "planner",
        RoleKind::Planner,
        &job,
        &EnvelopeError::NoOpening,
        1,
        300,
    );
    assert_eq!(got.budget, 300);
    assert_eq!(got.from_ref.as_deref(), Some("main"));
    assert_eq!(got.extra_targets, vec!["vivi-server".to_owned()]);
    assert_eq!(got.with, vec!["need/x".to_owned()]);
    assert_eq!(got.builtin_role.as_deref(), Some("planner"));
    assert_eq!(got.model.as_deref(), Some("m1"));
    assert!(got.task.is_none());
}

#[test]
fn a_correction_of_a_correction_keeps_one_base_id() {
    let job = original("j1-c1", "x");
    let got = spec(
        "n1",
        &row("j1-c1", 1),
        "planner",
        RoleKind::Planner,
        &job,
        &EnvelopeError::Unclosed,
        2,
        300,
    );
    assert_eq!(got.id, "j1-c2");
    assert_eq!(base_id("j1-c2"), "j1");
    assert_eq!(base_id("slice-t-a1-s2"), "slice-t-a1-s2");
    assert_eq!(correction_n("j1"), 0);
    assert_eq!(correction_n("j1-c2"), 2);
    assert_eq!(correction_n("sync-c"), 0);
}

#[test]
fn the_original_message_is_capped_at_16_kib() {
    let long = "é".repeat(20_000);
    let job = original("j1", &long);
    let got = spec(
        "n1",
        &row("j1", 1),
        "planner",
        RoleKind::Planner,
        &job,
        &EnvelopeError::NoOpening,
        1,
        300,
    );
    assert!(got.body.contains("[cut at 16384 bytes]"));
    assert!(got.body.len() < 17_000 + 2_000);
}

#[test]
fn enqueue_records_the_job_under_the_steps_own_task_and_attempt() {
    let dir = tempdir().unwrap();
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).unwrap();
    let mut jobs = Store::default();
    let job = original("j1", "x");
    let source = row("j1", 2);
    let fix = spec(
        "n1",
        &source,
        "planner",
        RoleKind::Planner,
        &job,
        &EnvelopeError::NoOpening,
        1,
        300,
    );
    enqueue(&mut jobs, &store, "n1", &source, fix).unwrap();
    assert!(jobs.jobs.contains_key("j1-c1"));
    assert!(jobs.jobs["j1-c1"].task.is_none());
    let recorded = store.step_job("j1-c1").unwrap().unwrap();
    assert_eq!(recorded.task, "t1");
    assert_eq!(recorded.node, "plan");
    assert_eq!(recorded.attempt, 2);
    let payload = &jobs.jobs["j1-c1"].payload;
    assert_eq!(payload["attempt"], 2);
    assert_eq!(payload["task"], "t1");
    assert_eq!(payload["node"], "plan");
    assert_eq!(payload["run"], 2);
    assert_eq!(payload["need"], "n1");
}

#[test]
fn the_guidance_reports_the_originals_discarded_evidence() {
    let mut job = original("j1", "the plan, no fences");
    job.read_only = true;
    job.outcome = Some(JobOutcome {
        state: "finished".to_owned(),
        detail: String::new(),
        receipt: "result: read_only_discarded 3\ndiscarded paths (3 of 3):\n  src/a.rs\n\n\
                  --- receipt\nthe plan, no fences\n"
            .to_owned(),
        at: 0,
    });
    let got = spec(
        "n1",
        &row("j1", 1),
        "planner",
        RoleKind::Planner,
        &job,
        &EnvelopeError::NoOpening,
        1,
        45,
    );
    assert!(got.body.contains("Diagnosis for job j1:"), "{}", got.body);
    assert!(got.body.contains("reply has no envelope"), "{}", got.body);
    assert!(
        got.body
            .contains("the read-only job changed 3 files (first: src/a.rs)"),
        "{}",
        got.body
    );
    assert!(
        got.body.contains("Rewrite only the envelope"),
        "{}",
        got.body
    );
}

#[test]
fn a_correction_reply_carries_its_own_discarded_evidence() {
    let mut job = original("j1-c1", "the corrected plan");
    job.read_only = true;
    job.outcome = Some(JobOutcome {
        state: "finished".to_owned(),
        detail: String::new(),
        receipt: "result: read_only_discarded 2\ndiscarded paths (2 of 2):\n  src/b.rs\n\n\
                  --- receipt\nthe corrected plan\n"
            .to_owned(),
        at: 0,
    });
    let got = spec(
        "n1",
        &row("j1-c1", 1),
        "planner",
        RoleKind::Planner,
        &job,
        &EnvelopeError::Unclosed,
        2,
        45,
    );
    assert_eq!(got.id, "j1-c2");
    assert!(
        got.body.contains("Diagnosis for job j1-c1:"),
        "{}",
        got.body
    );
    assert!(
        got.body
            .contains("the read-only job changed 2 files (first: src/b.rs)"),
        "{}",
        got.body
    );
    assert!(got.body.contains("the corrected plan"), "{}", got.body);
}

#[test]
fn a_job_is_never_resumable() {
    assert!(!resumable(&original("j1", "x")));
}
