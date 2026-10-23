//! A green main rung answers the unrepaired step of the matching repair run.

use tempfile::{TempDir, tempdir};

use super::{UNREPAIRED_NODE, answer_green};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{
    FlowDef, FlowEdge, FlowKind, FlowNode, NodeClass, SENTINEL_MARKER, STEP_MARKER,
};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, NeedStatus};
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const TIP: &str = "0123456789abcdef0123456789abcdef01234567";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    def: FlowDef,
}

fn node(id: &str, class: NodeClass) -> FlowNode {
    FlowNode {
        id: id.into(),
        class,
        label: id.into(),
    }
}

fn repair_def() -> FlowDef {
    FlowDef {
        kind: FlowKind::Repair,
        nodes: vec![
            node(UNREPAIRED_NODE, NodeClass::Lock),
            node("done", NodeClass::Done),
        ],
        edges: vec![FlowEdge {
            from: UNREPAIRED_NODE.into(),
            to: "done".into(),
            words: vec![String::from("green")],
        }],
        entries: vec![UNREPAIRED_NODE.into()],
    }
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail: FakeMail::new("pump"),
        def: repair_def(),
    })
}

impl Rig {
    /// A live run of `kind` on `need` with an unrepaired step Waiting and an
    /// open record naming `target`. Returns the unrepaired step task.
    fn run(&mut self, need: &str, kind: &str, target: &str) -> Result<String, VivariumError> {
        self.store.insert_run(&RunRow {
            need: need.into(),
            run: 1,
            flow: kind.into(),
            flow_version: 1,
            kind: kind.into(),
            entry: UNREPAIRED_NODE.into(),
            state: "live".into(),
        })?;
        let sentinel = self.mail.send_step(
            "pump",
            "pump",
            "run",
            &format!("{SENTINEL_MARKER} {need} r1"),
            &[],
        )?;
        self.mail.bind(need, std::slice::from_ref(&sentinel))?;
        self.store.insert_step(
            need,
            &StepRow {
                run: 1,
                node: "sentinel".into(),
                task: sentinel,
                state: StepState::Running,
                verdict: None,
            },
        )?;
        let task = self.mail.send_step(
            "pump",
            "pump",
            UNREPAIRED_NODE,
            &format!("{STEP_MARKER} {need} r1 {UNREPAIRED_NODE}"),
            &[],
        )?;
        self.mail.bind(need, std::slice::from_ref(&task))?;
        self.store.insert_step(
            need,
            &StepRow {
                run: 1,
                node: UNREPAIRED_NODE.into(),
                task: task.clone(),
                state: StepState::Waiting,
                verdict: None,
            },
        )?;
        MailWriter::open_record(
            &mut self.mail,
            need,
            &format!("kind: {kind}\ntarget: {target}"),
        )?;
        Ok(task)
    }

    fn green(&mut self, target: &str) -> Result<Vec<String>, VivariumError> {
        answer_green(
            &self.ctx,
            &self.store,
            &mut self.mail,
            &self.def,
            target,
            TIP,
        )
    }
}

#[test]
fn green_answers_the_unrepaired_step_and_lands_the_need() -> R {
    let mut rig = rig()?;
    let task = rig.run("n1", "repair", "default")?;

    assert_eq!(rig.green("default")?, vec![task]);

    assert_eq!(rig.mail.need_state("n1")?, NeedStatus::Done);
    assert_eq!(rig.mail.record_outcome("n1"), Some("landed"));
    assert!(rig.store.live_runs()?.is_empty());
    Ok(())
}

#[test]
fn another_target_is_untouched() -> R {
    let mut rig = rig()?;
    let task = rig.run("n1", "repair", "other")?;

    assert!(rig.green("default")?.is_empty());

    assert_ne!(rig.mail.need_state("n1")?, NeedStatus::Done);
    assert_eq!(rig.mail.record_outcome("n1"), None);
    assert_eq!(rig.store.live_runs()?.len(), 1);
    assert!(
        rig.store
            .steps_of("n1", 1)?
            .iter()
            .any(|s| s.task == task && s.state == StepState::Waiting)
    );
    Ok(())
}

#[test]
fn a_build_run_is_untouched() -> R {
    let mut rig = rig()?;
    rig.run("n1", "build", "default")?;

    assert!(rig.green("default")?.is_empty());

    assert_ne!(rig.mail.need_state("n1")?, NeedStatus::Done);
    assert_eq!(rig.store.live_runs()?.len(), 1);
    Ok(())
}
