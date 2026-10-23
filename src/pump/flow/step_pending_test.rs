//! The step-pending phase: a hold past budget plus grace raises once.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{GRACE_SECS, STEP_PENDING, register};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::SENTINEL_NODE;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::fanout::{VERDICT_MERGED, slice_node};
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::reslice;
use crate::pump::flow::scope::ScopeState;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::flow_links::SliceLink;
use crate::pump::store::{DB_FILENAME, PumpStore};

const NEED: &str = "n1";

#[derive(Default)]
struct Jobs {
    jobs: BTreeMap<String, PoolJob>,
}

impl JobStore for Jobs {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.jobs
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.jobs
    }
}

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
    registry: Registry<Jobs, FakeMail>,
}

fn rig(tag: &str) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut registry = Registry::new();
    register(&mut registry);
    store.insert_run(&RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    })?;
    for (node, task, state) in [
        (SENTINEL_NODE, format!("S-{tag}"), StepState::Pending),
        ("plan", format!("P-{tag}"), StepState::Pending),
        ("admit", format!("A-{tag}"), StepState::Running),
    ] {
        store.insert_step(
            NEED,
            &StepRow {
                run: 1,
                node: node.into(),
                task,
                state,
                verdict: None,
            },
        )?;
    }
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
        store,
        mail: FakeMail::new("mind"),
        jobs: Jobs::default(),
        registry,
    })
}

impl Rig {
    fn tick(&mut self) -> Result<(), VivariumError> {
        for phase in self.registry.phases() {
            phase.run(&self.ctx, &mut self.jobs, &self.store, &mut self.mail)?;
        }
        Ok(())
    }

    fn raised(&self) -> usize {
        fs::read_to_string(self.ctx.home.join("exceptions.txt"))
            .unwrap_or_default()
            .matches(STEP_PENDING)
            .count()
    }

    fn pending_job(&mut self, id: &str, kind: &str) {
        let mut spec = JobSpec::new(id, kind, "hand", "subject", "body");
        spec.budget = self.ctx.cfg.dispatch.budget;
        let mut job = PoolJob::from(spec);
        job.status = JobStatus::Pending;
        job.queued_at = self.ctx.now();
        self.jobs.jobs.insert(id.into(), job);
    }
}

fn past_limit(rig: &Rig) -> u64 {
    rig.ctx.cfg.dispatch.budget + GRACE_SECS + 1
}

#[test]
fn a_pending_row_raises_after_budget_plus_grace_and_only_once() -> Result<(), VivariumError> {
    let mut rig = rig("row")?;
    rig.tick()?;
    assert_eq!(rig.raised(), 0);
    rig.host.advance(past_limit(&rig) - 2);
    rig.tick()?;
    assert_eq!(rig.raised(), 0, "a fresh hold does not raise");
    rig.host.advance(2);
    rig.tick()?;
    assert_eq!(rig.raised(), 1);
    rig.tick()?;
    rig.host.advance(100);
    rig.tick()?;
    assert_eq!(rig.raised(), 1, "raised once");
    Ok(())
}

#[test]
fn the_sentinel_and_running_rows_never_raise() -> Result<(), VivariumError> {
    let mut rig = rig("quiet")?;
    rig.store
        .set_step_state("P-quiet", StepState::Running, None)?;
    rig.tick()?;
    rig.host.advance(past_limit(&rig));
    rig.tick()?;
    assert_eq!(rig.raised(), 0);
    Ok(())
}

#[test]
fn a_pending_job_of_the_run_raises_past_its_budget_but_a_fresh_one_does_not()
-> Result<(), VivariumError> {
    let mut rig = rig("job")?;
    rig.store
        .set_step_state("P-job", StepState::Running, None)?;
    rig.pending_job("old-job", "hand");
    rig.store.insert_step_job(
        NEED,
        &StepJobRow {
            job: "old-job".into(),
            run: 1,
            node: "plan".into(),
            task: "P-job".into(),
            attempt: 1,
        },
    )?;
    rig.tick()?;
    assert_eq!(rig.raised(), 0);
    rig.host.advance(past_limit(&rig));
    rig.pending_job("fresh-job", "hand");
    rig.store.insert_step_job(
        NEED,
        &StepJobRow {
            job: "fresh-job".into(),
            run: 1,
            node: "plan".into(),
            task: "P-job".into(),
            attempt: 2,
        },
    )?;
    rig.tick()?;
    assert_eq!(rig.raised(), 1);
    let text = fs::read_to_string(rig.ctx.home.join("exceptions.txt")).unwrap_or_default();
    assert!(text.contains("old-job"));
    assert!(!text.contains("fresh-job"));
    Ok(())
}

#[test]
fn a_held_overdue_job_raises_no_step_pending() -> Result<(), VivariumError> {
    let mut rig = rig("held")?;
    rig.store
        .set_step_state("P-held", StepState::Running, None)?;
    rig.pending_job("held-job", "hand");
    rig.store.insert_step_job(
        NEED,
        &StepJobRow {
            job: "held-job".into(),
            run: 1,
            node: "plan".into(),
            task: "P-held".into(),
            attempt: 1,
        },
    )?;
    if let Some(job) = rig.jobs.jobs.get_mut("held-job") {
        job.held = Some(String::from("no usable bridge"));
    }
    rig.host.advance(past_limit(&rig));
    rig.tick()?;
    assert_eq!(rig.raised(), 0);
    Ok(())
}

#[test]
fn a_reslice_job_of_the_run_counts_and_another_runs_job_does_not() -> Result<(), VivariumError> {
    let mut rig = rig("reslice")?;
    rig.store
        .set_step_state("P-reslice", StepState::Running, None)?;
    rig.pending_job("reslice-ours", reslice::KIND);
    rig.pending_job("reslice-other", reslice::KIND);
    rig.pending_job("stray-hand-job", "hand");
    if let Some(job) = rig.jobs.jobs.get_mut("reslice-ours") {
        job.payload = serde_json::json!({"need": NEED, "run": 1});
    }
    if let Some(job) = rig.jobs.jobs.get_mut("reslice-other") {
        job.payload = serde_json::json!({"need": "n2", "run": 1});
    }
    rig.host.advance(past_limit(&rig));
    rig.tick()?;
    let text = fs::read_to_string(rig.ctx.home.join("exceptions.txt")).unwrap_or_default();
    assert!(text.contains("reslice-ours"));
    assert!(!text.contains("reslice-other"));
    assert!(!text.contains("stray-hand-job"));
    assert_eq!(rig.raised(), 1);
    Ok(())
}

/// A Pending slice row `id` on a sent task, linked to `deps`; returns the task.
fn slice(rig: &mut Rig, id: &str, deps: &[String]) -> Result<String, VivariumError> {
    let task = rig.mail.send_step("mind", "mind", id, "body", &[])?;
    rig.store.insert_step(
        NEED,
        &StepRow {
            run: 1,
            node: slice_node(id),
            task: task.clone(),
            state: StepState::Pending,
            verdict: None,
        },
    )?;
    rig.store.link_slice(&SliceLink {
        task: task.clone(),
        need: NEED.into(),
        run: 1,
        fanout: None,
        deps: deps.to_vec(),
    })?;
    Ok(task)
}

fn settle_plain_rows(rig: &Rig) -> Result<(), VivariumError> {
    for task in ["P-s", "A-s"] {
        rig.store.set_step_state(task, StepState::Running, None)?;
    }
    Ok(())
}

#[test]
fn a_slice_waiting_on_an_open_dep_raises_nothing_until_the_dep_closes_merged()
-> Result<(), VivariumError> {
    let mut rig = rig("s")?;
    settle_plain_rows(&rig)?;
    let dep = slice(&mut rig, "dep", &[])?;
    rig.store.set_step_state(&dep, StepState::Running, None)?;
    slice(&mut rig, "late", std::slice::from_ref(&dep))?;
    rig.tick()?;
    rig.host.advance(past_limit(&rig));
    rig.tick()?;
    rig.host.advance(past_limit(&rig));
    rig.tick()?;
    assert_eq!(rig.raised(), 0, "a waiting slice is not held");
    rig.mail.close_task(&dep, VERDICT_MERGED, None, None)?;
    rig.tick()?;
    rig.host.advance(past_limit(&rig) - 2);
    rig.tick()?;
    assert_eq!(rig.raised(), 0, "the budget starts when the slice is ready");
    rig.host.advance(2);
    rig.tick()?;
    assert_eq!(rig.raised(), 1);
    rig.tick()?;
    assert_eq!(rig.raised(), 1, "raised once");
    Ok(())
}

#[test]
fn a_dep_closed_with_another_verdict_keeps_the_slice_waiting() -> Result<(), VivariumError> {
    let mut rig = rig("s")?;
    settle_plain_rows(&rig)?;
    let dep = slice(&mut rig, "dep", &[])?;
    rig.store.set_step_state(&dep, StepState::Running, None)?;
    slice(&mut rig, "late", std::slice::from_ref(&dep))?;
    rig.mail.close_task(&dep, "failed", None, None)?;
    rig.tick()?;
    rig.host.advance(past_limit(&rig));
    rig.tick()?;
    assert_eq!(rig.raised(), 0);
    Ok(())
}

#[test]
fn a_scope_wait_slice_raises_nothing() -> Result<(), VivariumError> {
    let mut rig = rig("s")?;
    settle_plain_rows(&rig)?;
    let wait = slice(&mut rig, "w", &[])?;
    let rework = slice(&mut rig, "r", &[])?;
    rig.store.insert_slice_scope(&wait, 1, &[])?;
    rig.store.insert_slice_scope(&rework, 1, &[])?;
    rig.store.set_scope_state(&wait, ScopeState::Wait)?;
    rig.store.set_scope_state(&rework, ScopeState::Rework)?;
    rig.tick()?;
    rig.host.advance(past_limit(&rig));
    rig.tick()?;
    assert_eq!(rig.raised(), 0);
    Ok(())
}

#[test]
fn a_ready_slice_and_a_non_slice_pending_row_still_raise() -> Result<(), VivariumError> {
    let mut rig = rig("s")?;
    slice(&mut rig, "free", &[])?;
    rig.tick()?;
    rig.host.advance(past_limit(&rig));
    rig.tick()?;
    assert_eq!(rig.raised(), 2, "the Pending plan row and the ready slice");
    Ok(())
}
