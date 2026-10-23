//! Tests for the step collector helpers: a rejected word holds the step, and
//! `rerun_with` edits the rerun's spec.

use std::collections::BTreeMap;
use std::fs;

use tempfile::{TempDir, tempdir};

use super::RunCx;
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::def::FlowDef;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::tick::{OPERATOR_HOLD, WORD_REJECTED};
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{self, JobSpec, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

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
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
    registry: Registry<Jobs, FakeMail>,
    def: FlowDef,
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, "kind: build")?;
    start(&ctx, &store, &mut mail, NEED, Some("plan"))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
        registry: Registry::new(),
        def: load(FlowKind::Build)?,
    })
}

impl Rig {
    fn plan_step(&self) -> Result<StepRow, VivariumError> {
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .find(|row| row.node == "plan")
            .ok_or_else(|| VivariumError::Message("no plan step".into()))
    }

    fn exceptions(&self) -> String {
        fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }
}

#[test]
fn a_word_outside_the_node_class_holds_the_step_for_the_operator() -> R {
    let mut rig = rig()?;
    let step = rig.plan_step()?;
    let run = rig.store.runs_of(NEED)?.remove(0);
    let decide = |_: &_, _: &_| Err(VivariumError::Message("no decision".into()));
    let mut cx = RunCx {
        ctx: &rig.ctx,
        st: &mut rig.jobs,
        store: &rig.store,
        mail: &mut rig.mail,
        registry: &rig.registry,
        decide: &decide,
        def: rig.def.clone(),
        run,
    };
    cx.advance_word(&step, "no-such-word", serde_json::json!({}))?;

    let held = rig.plan_step()?;
    assert_eq!(held.state, StepState::Waiting);
    assert_eq!(held.verdict.as_deref(), Some(OPERATOR_HOLD));
    let log = rig.exceptions();
    assert_eq!(log.matches(WORD_REJECTED).count(), 1);
    assert!(log.contains("held for vivi pump answer"), "{log}");
    Ok(())
}

#[test]
fn rerun_with_applies_the_edit_and_inserts_the_next_attempt() -> R {
    let mut rig = rig()?;
    let step = rig.plan_step()?;
    let mut spec = JobSpec::new("plan-j1", "planner", "pump-planner", "s", "b");
    spec.task = Some(step.task.clone());
    spec.branch = Some("vivi/repair/x".into());
    jobs::enqueue(&mut rig.jobs, spec)?;
    let row = StepJobRow {
        job: "plan-j1".into(),
        run: 1,
        node: "plan".into(),
        task: step.task.clone(),
        attempt: 1,
    };
    rig.store.insert_step_job(NEED, &row)?;
    let job = rig.jobs.jobs["plan-j1"].clone();
    let run = rig.store.runs_of(NEED)?.remove(0);
    let decide = |_: &_, _: &_| Err(VivariumError::Message("no decision".into()));
    let mut cx = RunCx {
        ctx: &rig.ctx,
        st: &mut rig.jobs,
        store: &rig.store,
        mail: &mut rig.mail,
        registry: &rig.registry,
        decide: &decide,
        def: rig.def.clone(),
        run,
    };
    cx.rerun_with(&job, &row, &step, |spec| {
        spec.branch = Some("vivi/repair/x-t2".into());
    })?;

    let queued = &rig.jobs.jobs["plan-j1-a2"];
    assert_eq!(queued.branch.as_deref(), Some("vivi/repair/x-t2"));
    assert_eq!(rig.store.step_job("plan-j1-a2")?.unwrap().attempt, 2);
    assert_eq!(rig.plan_step()?.state, StepState::Running);
    Ok(())
}

fn merge_job(publish_ref: Option<&str>, target: Option<&str>) -> PoolJob {
    let mut spec = JobSpec::new("j1", "merge", "merge", "s", "b");
    spec.publish_ref = publish_ref.map(str::to_owned);
    spec.target = target.map(str::to_owned);
    let mut jobs = Jobs::default();
    assert!(jobs::enqueue(&mut jobs, spec).is_ok());
    jobs.jobs["j1"].clone()
}

#[test]
fn a_merged_payload_names_the_branch_from_the_publish_ref() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let job = merge_job(Some("refs/heads/vivi/need/n1"), Some("default"));
    let payload = super::merged_payload(&ctx, &job, "abc");
    assert_eq!(
        payload,
        serde_json::json!({"sha": "abc", "commit": "abc", "branch": "vivi/need/n1"})
    );
}

#[test]
fn a_merged_payload_without_a_publish_ref_names_the_targets_main() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let expected = ctx.for_target("default").unwrap().cfg.main.clone();
    let job = merge_job(None, Some("default"));
    let payload = super::merged_payload(&ctx, &job, "abc");
    assert_eq!(payload["branch"], serde_json::json!(expected));
    assert_eq!(payload["commit"], serde_json::json!("abc"));
}
