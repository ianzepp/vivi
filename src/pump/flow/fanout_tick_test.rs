//! A fanout settles only while its own step is open: after rework1 routes on
//! to build2, build1's slices (all merged) must not answer for build2.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use crate::error::VivariumError;
use crate::letter::fixture::step_body;
use crate::pump::config::PumpConfig;
use crate::pump::envelope::roles::Slice;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::fanout::{self, VERDICT_MERGED, slice_node};
use crate::pump::flow::handlers;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::flow::step_inputs::POOL_RESULT_MARKER;
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

    fn row(&self, node: &str) -> Result<StepRow, VivariumError> {
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .find(|row| row.node == node)
            .ok_or_else(|| VivariumError::Message(format!("no step {node}")))
    }

    /// Send a step task for `node` and index its row, as routing leaves it.
    fn step(
        &mut self,
        node: &str,
        state: StepState,
        verdict: Option<&str>,
    ) -> Result<String, VivariumError> {
        let body = step_body(NEED, 1, node, "");
        let task = self.mail.send_step("mind", "mind", node, &body, &[])?;
        self.store.insert_step(
            NEED,
            &StepRow {
                run: 1,
                node: node.into(),
                task: task.clone(),
                state,
                verdict: verdict.map(str::to_string),
            },
        )?;
        Ok(task)
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

fn slicer_reply(id: &str) -> String {
    format!(
        "{POOL_RESULT_MARKER}j1: finished\npool p  slot 1  model m\n\n--- receipt\n\
         +++\nanswer = \"sliced\"\n\n[[slice]]\nid = \"{id}\"\ntitle = \"t\"\nbrief = 'b'\n\
         target = \"default\"\nwrite_scope = [\"src/{id}.rs\"]\ndeps = []\n+++\nsliced"
    )
}

/// A live build run where build1 is Done `built` with its one slice merged,
/// rework1 is Done `sliced` with a slicer reply, and build2 is Pending.
fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, "kind: build")?;
    let run = start(&ctx, &store, &mut mail, NEED, Some("slice"))?;
    let mut rig = Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
    };
    let slicer = rig.row("slice")?;
    rig.store
        .set_step_state(&slicer.task, StepState::Done, Some("sliced"))?;

    let build1 = rig.step("build1", StepState::Done, Some("built"))?;
    rig.store.set_open_fanout(NEED, 1, &build1)?;
    fanout::expand(
        &rig.ctx,
        &rig.store,
        &mut rig.mail,
        &run,
        &slicer,
        &[slice("a")],
    )?;
    let a = rig.row(&slice_node("a"))?;
    fanout::close_slice(&rig.store, &mut rig.mail, &a.task, VERDICT_MERGED)?;

    let rework1 = rig.step("rework1", StepState::Done, Some("sliced"))?;
    rig.mail.reply(&rework1, &slicer_reply("r"))?;
    rig.step("build2", StepState::Pending, None)?;
    Ok(rig)
}

#[test]
fn build2_waits_for_its_own_slices_after_build1_settled() -> R {
    let mut rig = rig()?;
    let build2 = rig.row("build2")?;

    // build1's slices are all merged, but its fanout step is Done: build2
    // must not answer for it. Its handler expands its own slice instead.
    rig.tick()?;
    assert_ne!(rig.row("build2")?.state, StepState::Done);
    assert_eq!(rig.store.open_fanout(NEED, 1)?, Some(build2.task.clone()));
    let r = rig.row(&slice_node("r"))?;

    rig.tick()?;
    assert_ne!(rig.row("build2")?.state, StepState::Done);

    // Its own slice closes merged: only now does build2 answer built.
    fanout::close_slice(&rig.store, &mut rig.mail, &r.task, VERDICT_MERGED)?;
    rig.tick()?;
    let build2 = rig.row("build2")?;
    assert_eq!(build2.state, StepState::Done);
    assert_eq!(build2.verdict.as_deref(), Some("built"));
    assert!(rig.mail.task_state(&r.task)?.verdict.is_some());
    Ok(())
}
