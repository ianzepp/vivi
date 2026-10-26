//! Tests for the envelope correction job.

use std::collections::BTreeMap;
use std::fs;

use tempfile::{TempDir, tempdir};

use super::{
    KIND, base_id, correction_n, corrects, enqueue, ordinal, original_id, resumable, spec,
};
use crate::pump::config::PumpConfig;
use crate::pump::envelope::EnvelopeError;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::flow::collect::{self, Collected};
use crate::pump::flow::def::NodeClass;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::StepJobRow;
use crate::pump::flow::slice_dispatch::{AttemptResult, classify, classify_corrected};
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::replay;
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

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

/// A finished correction of `base` at ordinal `n`, replaced in transport
/// under a requeue id the way the operator requeue builds one: the pool's
/// replay copy (payload rides along), finished, plus its replacement stamp.
fn replaced_correction(base: &str, n: u32, message: &str) -> PoolJob {
    let first = spec(
        "n1",
        &row(base, 1),
        "planner",
        RoleKind::Planner,
        &original(base, "broken beyond parsing"),
        &EnvelopeError::NoOpening,
        n,
        300,
    );
    let source = PoolJob::from(first);
    let mut replaced = PoolJob::from(replay::copy_spec(&source, format!("{base}-c{n}-q1")));
    finish(&mut replaced, message);
    replay::stamp_replacement(&mut replaced.payload, &source);
    replaced
}

/// Mark `job` finished with a worker-success receipt whose reply envelope
/// is `message`; every stamped payload key is kept.
fn finish(job: &mut PoolJob, message: &str) {
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".to_owned(),
        detail: String::new(),
        receipt: format!(
            "pool result j: finished\nagent exit 0 in 1s\npush: no_changes\n\n--- receipt\n{message}"
        ),
        at: 0,
    });
}

/// A finished hand job whose runner receipt shows the agent ran and pushed.
fn hand_facts(id: &str) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new(id, "hand", "hand", "s", "b"));
    job.task = Some("t1".to_owned());
    finish(&mut job, "the plan, no fences");
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
    // The spec's own payload carries only the correction ancestry; the step
    // identity is stamped later, by enqueue.
    assert_eq!(got.payload["correction_original_job"], "j1");
    assert_eq!(got.payload["correction_ordinal"], 1);
    assert_eq!(got.payload.as_object().map(serde_json::Map::len), Some(2));
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
    // The row stamp keeps the correction ancestry riding next to it.
    assert_eq!(payload["correction_original_job"], "j1");
    assert_eq!(payload["correction_ordinal"], 1);
}

#[test]
fn a_job_is_never_resumable() {
    assert!(!resumable(&original("j1", "x")));
}

// ---- ancestry across a transport replacement ------------------------------

#[test]
fn a_transport_replacement_of_a_correction_keeps_its_ancestry_and_ordinal() {
    let first = spec(
        "n1",
        &row("j1", 1),
        "planner",
        RoleKind::Planner,
        &original("j1", "the plan, no fences"),
        &EnvelopeError::NoOpening,
        1,
        300,
    );
    assert_eq!(first.id, "j1-c1");
    assert_eq!(first.payload["correction_original_job"], "j1");
    assert_eq!(first.payload["correction_ordinal"], 1);

    // The pool replaces the correction under a requeue id: the payload and
    // the replacement stamp ride the replay copy.
    let source = PoolJob::from(first);
    let mut replaced = PoolJob::from(replay::copy_spec(&source, "j1-c1-q1"));
    replay::stamp_replacement(&mut replaced.payload, &source);
    assert_eq!(replaced.payload["replaces"], "j1-c1");
    assert_eq!(replaced.payload["replacement_order"], 1);
    // The id alone now hides the chain; the stamp still answers for it.
    assert_eq!(correction_n(&replaced.id), 0);
    assert_eq!(ordinal(&replaced), 1);
    assert_eq!(corrects(&replaced).as_deref(), Some("j1"));
    assert_eq!(original_id(&replaced), "j1");

    // The next correction is the base's `-c2`, not the replacement's, and
    // keeps the same substantive ancestry one generation on.
    let second = spec(
        "n1",
        &row("j1-c1-q1", 1),
        "planner",
        RoleKind::Planner,
        &replaced,
        &EnvelopeError::Unclosed,
        ordinal(&replaced) + 1,
        300,
    );
    assert_eq!(second.id, "j1-c2");
    assert_eq!(base_id(&second.id), "j1");
    assert_eq!(second.payload["correction_original_job"], "j1");
    assert_eq!(second.payload["correction_ordinal"], 2);
    assert_eq!(corrects(&PoolJob::from(second)).as_deref(), Some("j1"));
}

#[test]
fn a_historical_correction_resolves_its_chain_from_the_id_alone() {
    let legacy = original("j1-c2", "still broken");
    assert_eq!(ordinal(&legacy), 2);
    assert_eq!(corrects(&legacy).as_deref(), Some("j1"));
    assert_eq!(original_id(&legacy), "j1");
    let plain = original("j1", "x");
    assert_eq!(ordinal(&plain), 0);
    assert_eq!(corrects(&plain), None);
    assert_eq!(original_id(&plain), "j1");
    // A requeue suffix alone is no chain: only `-cN` reads as a correction.
    let requeued = original("j1-q1", "x");
    assert_eq!(ordinal(&requeued), 0);
    assert_eq!(corrects(&requeued), None);
    assert_eq!(correction_n("sync-c1-a2"), 0);
}

// ---- the correction cap through role and hand parsing ---------------------

fn rig() -> (TempDir, Ctx, FakeMail) {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let mail = FakeMail::new("pump");
    (dir, ctx, mail)
}

fn exceptions(ctx: &Ctx) -> String {
    fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap_or_default()
}

#[test]
fn the_cap_counts_a_replaced_correction_through_role_parsing() {
    let (_dir, ctx, mut mail) = rig();
    // First correction, replaced in transport: one correction is spent, so
    // a still-broken reply asks for the second correction.
    let first = replaced_correction("j1", 1, "no envelope here");
    let got = collect::collect(
        &ctx,
        &mut mail,
        &first,
        &row(&first.id, 1),
        &NodeClass::Planner,
        "",
    );
    assert!(matches!(got, Collected::Correct { .. }), "{got:?}");

    // The second correction, replaced too: the suffix alone would read as
    // fresh work, the stamp spends the chain's last correction.
    let second = replaced_correction("j1", 2, "no envelope here");
    assert_eq!(correction_n(&second.id), 0);
    let got = collect::collect(
        &ctx,
        &mut mail,
        &second,
        &row(&second.id, 1),
        &NodeClass::Planner,
        "",
    );
    assert_eq!(got, Collected::Failed);
    assert!(exceptions(&ctx).contains("flow-envelope-failed"));

    // A historical correction without a stamp still counts its `-cN` id.
    let mut spent = original("j1-c2", "no envelope here");
    finish(&mut spent, "no envelope here");
    let got = collect::collect(
        &ctx,
        &mut mail,
        &spent,
        &row("j1-c2", 1),
        &NodeClass::Planner,
        "",
    );
    assert_eq!(got, Collected::Failed);
}

#[test]
fn the_cap_counts_a_replaced_correction_through_hand_parsing() {
    let (_dir, ctx, _mail) = rig();
    let hand = hand_facts("slice-t1-a1");
    // First correction, replaced in transport: the chain has one left.
    let reply = replaced_correction("slice-t1-a1", 1, "still no envelope");
    let got = classify_corrected(&ctx, &hand, &reply);
    assert!(
        matches!(got, Some(AttemptResult::Correct { .. })),
        "{got:?}"
    );
    // Second correction, replaced too: the cap is spent, not reset.
    let reply = replaced_correction("slice-t1-a1", 2, "still no envelope");
    assert_eq!(
        classify_corrected(&ctx, &hand, &reply),
        Some(AttemptResult::Failed)
    );
    assert!(exceptions(&ctx).contains("flow-envelope-failed"));
}

#[test]
fn malformed_content_still_uses_corrections() {
    let (_dir, ctx, mut mail) = rig();
    // Nonempty malformed content: role parsing asks for a correction, never
    // an infrastructure replay of the work.
    let mut job = original("j1", "a preamble paragraph, and no envelope at all");
    finish(&mut job, "a preamble paragraph, and no envelope at all");
    let got = collect::collect(
        &ctx,
        &mut mail,
        &job,
        &row("j1", 1),
        &NodeClass::Planner,
        "",
    );
    let Collected::Correct { error } = got else {
        panic!("expected a correction, got {got:?}")
    };
    // The correction is the envelope-fix job at the same attempt.
    let fix = spec(
        "n1",
        &row("j1", 1),
        "planner",
        RoleKind::Planner,
        &job,
        &error,
        ordinal(&job) + 1,
        300,
    );
    assert_eq!(fix.id, "j1-c1");
    assert_eq!(fix.kind, KIND);
    assert!(fix.read_only);
    // Hand parsing answers the same way for the same content.
    let mut hand = hand_facts("slice-t1-a1");
    finish(&mut hand, "prose only, nothing that parses");
    let got = classify(&ctx, &hand);
    assert!(
        matches!(got, Some(AttemptResult::Correct { .. })),
        "{got:?}"
    );
}
