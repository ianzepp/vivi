//! Tests for `finish` (through `advance`) and [`super::withdraw`] over the
//! fake mail port and a real pump store.

use serde_json::json;
use tempfile::{TempDir, tempdir};

use super::withdraw;
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::route::JoinInput;
use crate::pump::flow::advance::terminal::Finished;
use crate::pump::flow::advance::{Advanced, advance};
use crate::pump::flow::def::{FlowDef, FlowKind, FlowNode, SENTINEL_MARKER, STEP_MARKER};
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailReader, MailWriter, NeedStatus, TaskStatus};
use crate::pump::flow::outcome::Outcome;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    def: FlowDef,
    sentinel: String,
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    store.insert_run(&run_row())?;
    let mut mail = FakeMail::new("pump");
    let sentinel = mail.send_step(
        "pump",
        "pump",
        "run",
        &format!("{SENTINEL_MARKER} {NEED} r1"),
        &[],
    )?;
    mail.bind(NEED, std::slice::from_ref(&sentinel))?;
    store.insert_step(
        NEED,
        &StepRow {
            run: 1,
            node: "sentinel".into(),
            task: sentinel.clone(),
            state: StepState::Running,
            verdict: None,
        },
    )?;
    MailWriter::open_record(&mut mail, NEED, "record")?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        def: load(FlowKind::Build)?,
        sentinel,
    })
}

fn run_row() -> RunRow {
    RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 1,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    }
}

impl Rig {
    /// An open step of `node` in the run, already bound.
    fn step(&mut self, node: &str, word: &str) -> Result<Outcome, VivariumError> {
        let task = self.mail.send_step(
            "pump",
            "pump",
            node,
            &format!("{STEP_MARKER} {NEED} r1 {node}"),
            &[],
        )?;
        self.mail.bind(NEED, std::slice::from_ref(&task))?;
        self.store.insert_step(
            NEED,
            &StepRow {
                run: 1,
                node: node.into(),
                task: task.clone(),
                state: StepState::Running,
                verdict: None,
            },
        )?;
        Ok(Outcome {
            need: NEED.into(),
            run: 1,
            node: node.into(),
            task,
            word: word.into(),
            payload: json!({}),
        })
    }

    fn advance(&mut self, out: &Outcome) -> Result<Advanced, VivariumError> {
        let decide = |_: &FlowNode, _: &[JoinInput]| Ok(String::new());
        advance(
            &self.ctx,
            &self.store,
            &mut self.mail,
            &self.def,
            out,
            &decide,
        )
    }
}

fn close_pos(rig: &Rig, task: &str) -> Option<usize> {
    rig.mail
        .position(|c| matches!(c, Call::CloseTask { handle, .. } if handle == task))
}

#[test]
fn done_closes_sentinel_then_need_then_record() -> R {
    let mut rig = rig()?;
    let plan = rig.step("plan", "delivered")?;
    rig.mail.calls.clear();

    let advanced = rig.advance(&plan)?;
    assert_eq!(advanced.finished, vec![Finished::Landed]);

    let step = close_pos(&rig, &plan.task).expect("step closed");
    let sentinel = close_pos(&rig, &rig.sentinel).expect("sentinel closed");
    let need = rig
        .mail
        .position(|c| matches!(c, Call::CloseNeed { .. }))
        .expect("need closed");
    let record = rig
        .mail
        .position(|c| matches!(c, Call::CloseRecord { outcome, .. } if outcome == "landed"))
        .expect("record closed");
    assert!(step < sentinel && sentinel < need && need < record);
    assert_eq!(
        rig.mail.task_state(&rig.sentinel)?.verdict.as_deref(),
        Some("delivered")
    );
    assert_eq!(rig.mail.need_state(NEED)?, NeedStatus::Done);
    assert_eq!(rig.mail.record_outcome(NEED), Some("landed"));
    assert!(rig.store.live_runs()?.is_empty(), "the run is not live");
    Ok(())
}

#[test]
fn restart_binds_the_new_sentinel_before_the_old_one_closes() -> R {
    let mut rig = rig()?;
    let cto = rig.step("cto", "repair")?;
    rig.mail.calls.clear();

    let advanced = rig.advance(&cto)?;
    let Some(Finished::Restarted(new_run)) = advanced.finished.first() else {
        panic!("expected a restart, got {:?}", advanced.finished);
    };
    // restart-repair maps to the declared entry repair1.
    assert_eq!((new_run.run, new_run.entry.as_str()), (2, "repair1"));

    let new_sentinel = rig
        .store
        .steps_of(NEED, 2)?
        .into_iter()
        .find(|r| r.node == "sentinel")
        .expect("new sentinel row")
        .task;
    let bind = rig
        .mail
        .position(|c| matches!(c, Call::Bind { units, .. } if units.contains(&new_sentinel)))
        .expect("new sentinel bound");
    let old_close = close_pos(&rig, &rig.sentinel).expect("old sentinel closed");
    assert!(bind < old_close);
    assert_eq!(
        rig.mail.task_state(&rig.sentinel)?.verdict.as_deref(),
        Some("superseded")
    );
    // The need stays open: the new sentinel is open.
    assert_eq!(rig.mail.need_state(NEED)?, NeedStatus::Open);
    assert_eq!(rig.mail.task_state(&new_sentinel)?.status, TaskStatus::Open);
    Ok(())
}

#[test]
fn withdrawal_closes_open_tasks_and_never_reopens_a_closed_need() -> R {
    let mut rig = rig()?;
    let a = rig.step("build1", "")?;
    let b = rig.step("audit1", "")?;
    // A task already closed stays as it is.
    let c = rig.step("test1", "")?;
    rig.mail.foreign_close(&c.task, "mind");
    // Someone closed the need directly.
    rig.mail.close_need_by(NEED, "mind");
    rig.mail.calls.clear();

    withdraw(&rig.store, &mut rig.mail, &run_row())?;

    for task in [&a.task, &b.task, &rig.sentinel] {
        let state = rig.mail.task_state(task)?;
        assert_eq!(state.status, TaskStatus::Done);
        assert_eq!(state.verdict.as_deref(), Some("withdrawn"));
    }
    assert_eq!(
        rig.mail.task_state(&c.task)?.verdict.as_deref(),
        Some("done")
    );
    let sentinel = close_pos(&rig, &rig.sentinel).expect("sentinel closed");
    assert!(close_pos(&rig, &a.task) < Some(sentinel));
    assert!(
        rig.mail
            .position(|c| matches!(c, Call::ReopenTask { .. }))
            .is_none()
    );
    assert_eq!(rig.mail.need_state(NEED)?, NeedStatus::Done);
    assert_eq!(rig.mail.need_closer(NEED), Some("mind"));
    assert_eq!(rig.mail.record_outcome(NEED), Some("withdrawn"));
    assert!(rig.store.live_runs()?.is_empty());
    let row = rig.store.step_by_task(&a.task)?.expect("row");
    assert_eq!(row.verdict.as_deref(), Some("withdrawn"));
    Ok(())
}
