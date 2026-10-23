//! A fanout whose slices all closed no-defect answers `unchanged`: the CTO
//! step opens and no audit, rung or merge step is sent.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use crate::error::VivariumError;
use crate::letter::fixture::step_body;
use crate::pump::config::PumpConfig;
use crate::pump::envelope::roles::Slice;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::fanout::{self, UNCHANGED, VERDICT_NO_DEFECT, slice_node};
use crate::pump::flow::handlers;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobStore, PoolJob};
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
}

impl Rig {
    fn tick(&mut self) -> R {
        let registry = handlers::shipped::<Jobs, FakeMail>(&self.ctx);
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
            &registry,
        )
    }

    fn find(&self, node: &str) -> Result<Option<StepRow>, VivariumError> {
        Ok(self
            .store
            .steps_of(NEED, 1)?
            .into_iter()
            .find(|row| row.node == node))
    }

    fn row(&self, node: &str) -> Result<StepRow, VivariumError> {
        self.find(node)?
            .ok_or_else(|| VivariumError::Message(format!("no step {node}")))
    }

    fn step(&mut self, node: &str, state: StepState) -> Result<String, VivariumError> {
        let body = step_body(NEED, 1, node, "");
        let task = self.mail.send_step("mind", "mind", node, &body, &[])?;
        self.store.insert_step(
            NEED,
            &StepRow {
                run: 1,
                node: node.into(),
                task: task.clone(),
                state,
                verdict: None,
            },
        )?;
        Ok(task)
    }

    /// Expand two slices under `fanout_task` and close both no-defect.
    fn expand_unchanged(&mut self, fanout_task: &str, ids: [&str; 2]) -> R {
        self.store.set_open_fanout(NEED, 1, fanout_task)?;
        let run = self
            .store
            .live_runs()?
            .into_iter()
            .next()
            .ok_or_else(|| VivariumError::Message("no live run".into()))?;
        let slicer = self.row("slice")?;
        let slices = [slice(ids[0]), slice(ids[1])];
        fanout::expand(
            &self.ctx,
            &self.store,
            &mut self.mail,
            &run,
            &slicer,
            &slices,
        )?;
        for id in ids {
            let task = self.row(&slice_node(id))?.task;
            fanout::close_slice(&self.store, &mut self.mail, &task, VERDICT_NO_DEFECT)?;
        }
        Ok(())
    }
}

fn slice(id: &str) -> Slice {
    Slice {
        id: id.into(),
        title: format!("title {id}"),
        brief: format!("brief {id}"),
        target: "default".into(),
        write_scope: vec![format!("src/{id}.rs")],
        deps: Vec::new(),
    }
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, "kind: build")?;
    start(&ctx, &store, &mut mail, NEED, Some("slice"))?;
    let slicer = store
        .steps_of(NEED, 1)?
        .into_iter()
        .find(|row| row.node == "slice")
        .ok_or_else(|| VivariumError::Message("no slice step".into()))?;
    store.set_step_state(&slicer.task, StepState::Done, Some("sliced"))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
    })
}

fn assert_no_verify_steps(rig: &Rig) -> R {
    for node in ["audit1", "test1", "audit2", "test2", "merge"] {
        assert!(rig.find(node)?.is_none(), "{node} must not exist");
    }
    Ok(())
}

#[test]
fn build1_all_no_defect_answers_unchanged_and_opens_the_cto() -> R {
    let mut rig = rig()?;
    let build1 = rig.step("build1", StepState::Running)?;
    rig.expand_unchanged(&build1, ["a", "b"])?;
    rig.tick()?;
    let row = rig.row("build1")?;
    assert_eq!(row.state, StepState::Done);
    assert_eq!(row.verdict.as_deref(), Some(UNCHANGED));
    assert!(rig.find("cto")?.is_some(), "the cto step is opened");
    assert_no_verify_steps(&rig)
}

#[test]
fn build2_all_no_defect_answers_unchanged_after_rework1() -> R {
    let mut rig = rig()?;
    let build1 = rig.step("build1", StepState::Done)?;
    rig.store
        .set_step_state(&build1, StepState::Done, Some("built"))?;
    let rework1 = rig.step("rework1", StepState::Done)?;
    rig.store
        .set_step_state(&rework1, StepState::Done, Some("sliced"))?;
    let build2 = rig.step("build2", StepState::Running)?;
    rig.expand_unchanged(&build2, ["c", "d"])?;
    rig.tick()?;
    let row = rig.row("build2")?;
    assert_eq!(row.state, StepState::Done);
    assert_eq!(row.verdict.as_deref(), Some(UNCHANGED));
    assert!(rig.find("cto")?.is_some(), "the cto step is opened");
    assert_no_verify_steps(&rig)
}
