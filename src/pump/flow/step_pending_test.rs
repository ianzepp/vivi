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
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::reslice;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobSpec, JobStatus, JobStore, PoolJob};
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
        flow_version: 1,
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
