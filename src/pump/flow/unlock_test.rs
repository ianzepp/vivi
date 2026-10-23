//! Tests for the lock handler and [`super::apply_pending`]: an unlock letter
//! releases the locked step and restarts the run once; a need with no locked
//! step or an unknown entry is refused with an alarm.

use std::fs;

use super::{LockWait, NEED_LOCKED, UNLOCK_REFUSED, apply_pending};
use crate::error::VivariumError;
use crate::letter::fixture::step_body;
use crate::mailspace::PumpActionRequest;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailReader, MailWriter, TaskStatus};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::host::Ctx;
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

struct Rig {
    project: Project,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    need: String,
}

/// A real need with an open pump record and a live run 1 whose entry step is
/// open; `locked` adds the `locked` step in state Waiting.
fn rig(locked: bool) -> Result<Rig, VivariumError> {
    let project = project();
    let ctx = project.pump_ctx();
    let store = PumpStore::open(&project.home().join(DB_FILENAME))?;
    let need = project.need("fix auth");
    project.mailspace.pump_send(None, &need, None)?;
    let mut mail = FakeMail::new("pump");
    MailWriter::open_record(&mut mail, &need, "record")?;
    start(&ctx, &store, &mut mail, &need, None)?;
    if locked {
        let task = mail.send_step(
            "pump",
            "pump",
            "locked",
            &step_body(&need, 1, "locked", ""),
            &[],
        )?;
        mail.bind(&need, std::slice::from_ref(&task))?;
        store.insert_step(
            &need,
            &StepRow {
                run: 1,
                node: "locked".into(),
                task,
                state: StepState::Waiting,
                verdict: None,
            },
        )?;
    }
    Ok(Rig {
        project,
        ctx,
        store,
        mail,
        need,
    })
}

impl Rig {
    fn post(&self, entry: Option<&str>) -> Result<String, VivariumError> {
        Ok(self
            .project
            .mailspace
            .pump_post_action(None, &self.need, &PumpActionRequest::unlock(entry, "ok"))?
            .id)
    }

    fn runs(&self) -> Result<Vec<RunRow>, VivariumError> {
        self.store.runs_of(&self.need)
    }

    fn row(&self, run: u32, node: &str) -> Result<StepRow, VivariumError> {
        self.store
            .steps_of(&self.need, run)?
            .into_iter()
            .find(|row| row.node == node)
            .ok_or_else(|| VivariumError::Other(format!("no step {node} in r{run}")))
    }

    fn exceptions(&self) -> String {
        fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }
}

#[test]
fn an_unlock_closes_the_lock_step_and_restarts_at_the_first_entry() -> R {
    let mut rig = rig(true)?;
    let id = rig.post(None)?;
    let locked = rig.row(1, "locked")?.task;

    apply_pending(&rig.ctx, &rig.store, &mut rig.mail)?;

    let closed = rig.row(1, "locked")?;
    assert_eq!(closed.state, StepState::Done);
    assert_eq!(closed.verdict.as_deref(), Some("unlocked"));
    let state = rig.mail.task_state(&locked)?;
    assert_eq!(state.status, TaskStatus::Done);
    assert_eq!(state.verdict.as_deref(), Some("unlocked"));
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].state, "superseded");
    assert_eq!(runs[1].run, 2);
    assert_eq!(runs[1].state, "live");
    assert_eq!(runs[1].entry, "plan");
    assert_eq!(rig.row(2, "plan")?.state, StepState::Pending);
    assert!(rig.store.applied_action_ids()?.contains(&id));
    assert_eq!(rig.exceptions(), "");

    apply_pending(&rig.ctx, &rig.store, &mut rig.mail)?;
    assert_eq!(rig.runs()?.len(), 2, "applied exactly once");
    Ok(())
}

#[test]
fn an_unlock_with_an_entry_restarts_there() -> R {
    let mut rig = rig(true)?;
    rig.post(Some("slice"))?;

    apply_pending(&rig.ctx, &rig.store, &mut rig.mail)?;

    let runs = rig.runs()?;
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].entry, "slice");
    assert_eq!(rig.row(2, "slice")?.state, StepState::Pending);
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn an_unlock_with_no_locked_step_is_refused_and_applied() -> R {
    let mut rig = rig(false)?;
    let id = rig.post(None)?;

    apply_pending(&rig.ctx, &rig.store, &mut rig.mail)?;
    apply_pending(&rig.ctx, &rig.store, &mut rig.mail)?;

    assert_eq!(rig.runs()?.len(), 1);
    assert_eq!(rig.runs()?[0].state, "live");
    let log = rig.exceptions();
    assert_eq!(log.matches(UNLOCK_REFUSED).count(), 1, "{log}");
    assert!(log.contains("has no locked step"), "{log}");
    assert!(rig.store.applied_action_ids()?.contains(&id));
    Ok(())
}

#[test]
fn an_unknown_entry_is_refused_and_the_lock_stays() -> R {
    let mut rig = rig(true)?;
    let id = rig.post(Some("nowhere"))?;

    apply_pending(&rig.ctx, &rig.store, &mut rig.mail)?;

    assert_eq!(rig.runs()?.len(), 1);
    assert_eq!(rig.row(1, "locked")?.state, StepState::Waiting);
    let log = rig.exceptions();
    assert!(log.contains(UNLOCK_REFUSED), "{log}");
    assert!(
        log.contains("`nowhere` is not an entry of the build flow"),
        "{log}"
    );
    assert!(rig.store.applied_action_ids()?.contains(&id));
    Ok(())
}

#[test]
fn the_lock_handler_opens_and_waits_and_a_poll_never_answers() -> R {
    let rig = rig(true)?;
    let def = load(FlowKind::Build)?;
    let run = rig.runs()?.remove(0);
    let step = rig.row(1, "locked")?;
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &run,
        def: &def,
    };

    assert_eq!(LockWait.start(&cx, &step)?, StartAction::OpenAndWait);
    assert_eq!(LockWait.poll(&cx, &step)?, None);
    Ok(())
}

#[test]
fn an_unlock_still_releases_the_lock_after_the_alarm_was_raised() -> R {
    let mut rig = rig(true)?;
    let def = load(FlowKind::Build)?;
    let run = rig.runs()?.remove(0);
    let step = rig.row(1, "locked")?;
    {
        let cx = StartCx {
            ctx: &rig.ctx,
            store: &rig.store,
            mail: &rig.mail,
            run: &run,
            def: &def,
        };
        assert_eq!(LockWait.poll(&cx, &step)?, None);
        assert_eq!(LockWait.poll(&cx, &step)?, None);
    }
    let log = rig.exceptions();
    assert_eq!(log.matches(NEED_LOCKED).count(), 1, "{log}");
    assert!(log.contains("reached without a CTO answer"), "{log}");
    assert!(log.contains(&rig.need), "{log}");

    rig.post(None)?;
    apply_pending(&rig.ctx, &rig.store, &mut rig.mail)?;

    assert_eq!(rig.row(1, "locked")?.state, StepState::Done);
    assert_eq!(rig.row(1, "locked")?.verdict.as_deref(), Some("unlocked"));
    assert_eq!(rig.runs()?.len(), 2);
    assert_eq!(rig.runs()?[1].state, "live");
    assert!(!rig.exceptions().contains(UNLOCK_REFUSED));
    Ok(())
}
