//! Tests for acting on a slicer reply for one slice.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{FOLLOW_UP_UNROUTED, apply};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::envelope::roles::{Slice, Slicer, SlicerAnswer, SlicerMode};
use crate::pump::flow::def::{SENTINEL_MARKER, STEP_MARKER};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::fanout::{self, slice_node};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, TaskStatus};
use crate::pump::flow::reslice::Mode;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    run: RunRow,
    reslice: StepRow,
    old: String,
    dependent: String,
}

fn slice(id: &str, deps: &[&str]) -> Slice {
    Slice {
        id: id.into(),
        title: format!("title {id}"),
        brief: format!("brief {id}"),
        target: "vivi".into(),
        write_scope: vec![format!("src/{id}.rs")],
        deps: deps.iter().map(|d| (*d).to_string()).collect(),
    }
}

fn step(run: u32, node: &str, task: &str, state: StepState) -> StepRow {
    StepRow {
        run,
        node: node.into(),
        task: task.into(),
        state,
        verdict: None,
    }
}

/// A run with slices A and B (B depends on A) and a reslice row for A.
fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let run = RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 1,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    };
    store.insert_run(&run)?;
    let mut mail = FakeMail::new(&ctx.cfg.reply_from);
    let sentinel = mail.send_step(
        "pump",
        "pump",
        "run",
        &format!("{SENTINEL_MARKER} {NEED} r1"),
        &[],
    )?;
    let slicer_task = mail.send_step(
        "pump",
        "pump",
        "slice",
        &format!("{STEP_MARKER} {NEED} r1 slice"),
        &[],
    )?;
    mail.bind(NEED, &[sentinel.clone(), slicer_task.clone()])?;
    store.insert_step(NEED, &step(1, "sentinel", &sentinel, StepState::Running))?;
    let slicer = step(1, "slice", &slicer_task, StepState::Done);
    store.insert_step(NEED, &slicer)?;
    let tasks = fanout::expand(
        &ctx,
        &store,
        &mut mail,
        &run,
        &slicer,
        &[slice("A", &[]), slice("B", &["A"])],
    )?;
    let reslice_task = mail.send_step(
        "pump",
        "pump",
        "reslice",
        &format!("{STEP_MARKER} {NEED} r1 reslice:A-1\nmode: split\n\nsplit it"),
        &[],
    )?;
    let reslice = step(1, "reslice:A-1", &reslice_task, StepState::Running);
    store.insert_step(NEED, &reslice)?;
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
        store,
        mail,
        run,
        reslice,
        old: tasks[0].clone(),
        dependent: tasks[1].clone(),
    })
}

fn sliced(mode: SlicerMode, slices: Vec<Slice>) -> Slicer {
    Slicer {
        answer: SlicerAnswer::Sliced,
        mode,
        slice: slices,
        reason: None,
    }
}

fn escalate() -> Slicer {
    Slicer {
        answer: SlicerAnswer::Escalate,
        mode: SlicerMode::Split,
        slice: Vec::new(),
        reason: Some("not this need".into()),
    }
}

impl Rig {
    fn apply(&mut self, mode: Mode, reply: Option<&Slicer>) -> Result<&'static str, VivariumError> {
        apply(
            &self.ctx,
            &self.store,
            &mut self.mail,
            &self.run,
            &self.reslice,
            &self.old.clone(),
            mode,
            reply,
        )
    }

    fn task_of(&self, id: &str) -> Result<String, VivariumError> {
        let node = slice_node(id);
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .find(|row| row.node == node)
            .map(|row| row.task)
            .ok_or_else(|| VivariumError::Message(format!("no slice row {id}")))
    }
}

#[test]
fn split_sliced_replaces_the_old_slice_and_rewires_the_dependent() -> R {
    let mut rig = rig()?;
    let reply = sliced(
        SlicerMode::Split,
        vec![slice("A1", &[]), slice("A2", &["A1"])],
    );
    assert_eq!(rig.apply(Mode::Split, Some(&reply))?, "sliced");

    let state = rig.mail.task_state(&rig.old)?;
    assert_eq!(state.status, TaskStatus::Done);
    assert_eq!(state.verdict.as_deref(), Some("replaced"));
    let a1 = rig.task_of("A1")?;
    let a2 = rig.task_of("A2")?;
    let link = rig.store.slice_link(&rig.dependent)?.expect("link");
    assert!(!link.deps.contains(&rig.old));
    assert!(link.deps.contains(&a1) && link.deps.contains(&a2));
    let edges = &rig.mail.tasks[&rig.dependent].depends_on;
    assert!(edges.contains(&a1) && edges.contains(&a2));
    Ok(())
}

#[test]
fn split_escalate_exhausts_the_old_slice() -> R {
    let mut rig = rig()?;
    let word = rig.apply(Mode::Split, Some(&escalate()))?;
    assert_eq!(word, "escalate");
    let row = rig.store.step_by_task(&rig.old)?.expect("row");
    assert_eq!(row.state, StepState::Failed);
    Ok(())
}

#[test]
fn rework_with_no_reply_exhausts_and_fails() -> R {
    let mut rig = rig()?;
    assert_eq!(rig.apply(Mode::Rework, None)?, "failed");
    let row = rig.store.step_by_task(&rig.old)?.expect("row");
    assert_eq!(row.state, StepState::Failed);
    Ok(())
}

#[test]
fn follow_up_sliced_adds_slices_and_leaves_the_old_slice_open() -> R {
    let mut rig = rig()?;
    let reply = sliced(SlicerMode::Rework, vec![slice("F1", &[])]);
    assert_eq!(rig.apply(Mode::FollowUp, Some(&reply))?, "sliced");
    rig.task_of("F1")?;
    let state = rig.mail.task_state(&rig.old)?;
    assert_ne!(state.status, TaskStatus::Done);
    let link = rig.store.slice_link(&rig.dependent)?.expect("link");
    assert!(link.deps.contains(&rig.old));
    Ok(())
}

#[test]
fn follow_up_escalate_files_one_need() -> R {
    let mut rig = rig()?;
    rig.host.reply("need send", 0, "created need x9\n");
    assert_eq!(rig.apply(Mode::FollowUp, Some(&escalate()))?, "escalate");
    let sends: Vec<String> = rig
        .host
        .commands()
        .into_iter()
        .filter(|c| c.contains("need send"))
        .collect();
    assert_eq!(sends.len(), 1);
    let row = rig.store.step_by_task(&rig.old)?.expect("row");
    assert_ne!(row.state, StepState::Failed);
    Ok(())
}

#[test]
fn follow_up_with_a_failed_send_raises_unrouted_and_returns_the_word() -> R {
    let mut rig = rig()?;
    rig.host.reply("need send", 1, "boom");
    assert_eq!(rig.apply(Mode::FollowUp, None)?, "failed");
    let text = std::fs::read_to_string(rig.ctx.home.join("exceptions.txt"))
        .map_err(|e| VivariumError::Other(e.to_string()))?;
    assert!(text.contains(FOLLOW_UP_UNROUTED), "{text}");
    Ok(())
}
