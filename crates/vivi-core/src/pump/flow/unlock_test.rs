//! Tests for the lock handler and [`super::apply_pending`]: an unlock letter
//! releases the locked step and restarts the run once; a need with no locked
//! step or an unknown entry is refused with an alarm.

use std::collections::BTreeMap;
use std::fs;

use super::{LockWait, NEED_LOCKED, UNLOCK_REFUSED, apply_pending};
use crate::mailspace::PumpActionRequest;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailReader, MailWriter, TaskStatus};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use vivi_error::VivariumError;
use vivi_mail::letter::fixture::step_body;

type R = Result<(), VivariumError>;

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

fn run_tuples(rig: &Rig) -> Result<Vec<(u32, String, String)>, VivariumError> {
    Ok(rig
        .runs()?
        .into_iter()
        .map(|run| (run.run, run.entry, run.state))
        .collect())
}

fn sentinel_task(rig: &Rig, run: u32) -> Result<String, VivariumError> {
    Ok(rig
        .store
        .steps_of(&rig.need, run)?
        .into_iter()
        .find(|row| row.node == "sentinel")
        .ok_or_else(|| VivariumError::Other(format!("no sentinel in r{run}")))?
        .task)
}

#[test]
fn a_blocked_unlock_keeps_one_intent_and_starts_on_admission() -> R {
    let mut rig = rig(true)?;
    rig.mail.set_prerequisites(&rig.need, &["dep"]);
    let id = rig.post(Some("slice"))?;
    let old_sentinel = sentinel_task(&rig, 1)?;

    apply_pending(&rig.ctx, &rig.store, &mut rig.mail)?;
    apply_pending(&rig.ctx, &rig.store, &mut rig.mail)?;

    assert!(rig.store.applied_action_ids()?.contains(&id));
    assert_eq!(run_tuples(&rig)?, [(1, "plan".into(), "restarting".into())]);
    assert_eq!(
        rig.row(1, "locked")?.verdict.as_deref(),
        Some("unlocked"),
        "the lock closes even while the successor waits"
    );
    assert_eq!(
        rig.store.restart_entry(&rig.need, 1)?.as_deref(),
        Some("slice"),
        "the requested entry is durable"
    );
    assert_eq!(
        rig.mail.task_state(&old_sentinel)?.status,
        TaskStatus::Open,
        "the old sentinel waits for the new binding"
    );
    assert_eq!(rig.store.build_waits()?.len(), 1, "the blocker is queued");
    assert!(!rig.exceptions().contains(UNLOCK_REFUSED));

    let reopened = PumpStore::open(&rig.project.home().join(DB_FILENAME))?;
    assert_eq!(
        reopened.restart_entry(&rig.need, 1)?.as_deref(),
        Some("slice"),
        "the requested entry survives a reopened store"
    );

    rig.mail.set_prerequisites(&rig.need, &[]);
    let mut jobs = Jobs::default();
    let mut registry = Registry::new();
    crate::pump::flow::intake::register(&mut registry);
    crate::pump::flow::advance::restart::register(&mut registry);
    crate::pump::flow::tick::tick_with(&rig.ctx, &mut jobs, &reopened, &mut rig.mail, &registry)?;

    assert_eq!(
        run_tuples(&rig)?,
        [
            (1, "plan".into(), "superseded".into()),
            (2, "slice".into(), "live".into())
        ],
        "exactly one successor at the saved entry"
    );
    let new_sentinel = sentinel_task(&rig, 2)?;
    assert_eq!(
        rig.mail.task_state(&old_sentinel)?.verdict.as_deref(),
        Some("superseded")
    );
    assert_eq!(rig.mail.task_state(&new_sentinel)?.status, TaskStatus::Open);
    let bind = rig
        .mail
        .position(|c| matches!(c, Call::Bind { units, .. } if units.contains(&new_sentinel)))
        .expect("the successor sentinel is bound");
    let close = rig
        .mail
        .position(|c| matches!(c, Call::CloseTask { handle, .. } if *handle == old_sentinel))
        .expect("the parent sentinel closes");
    assert!(bind < close, "the new sentinel binds before the old closes");
    assert!(rig.store.build_waits()?.is_empty());

    apply_pending(&rig.ctx, &reopened, &mut rig.mail)?;
    assert_eq!(rig.runs()?.len(), 2, "the action is never applied twice");
    Ok(())
}

#[test]
fn a_failed_start_after_the_lock_close_keeps_the_requested_entry() -> R {
    let mut rig = rig(true)?;
    let id = rig.post(Some("slice"))?;
    // The lock close is the next mail write; the successor's first send is
    // the one after it. Fail that later write.
    rig.mail.fail_at = Some(rig.mail.calls.len() + 1);

    apply_pending(&rig.ctx, &rig.store, &mut rig.mail)?;

    assert!(rig.store.applied_action_ids()?.contains(&id));
    assert!(rig.exceptions().contains(UNLOCK_REFUSED));
    assert_eq!(
        rig.row(1, "locked")?.verdict.as_deref(),
        Some("unlocked"),
        "the accepted unlock still closes its lock step"
    );
    let reopened = PumpStore::open(&rig.project.home().join(DB_FILENAME))?;
    assert_eq!(
        reopened.restart_entry(&rig.need, 1)?.as_deref(),
        Some("slice"),
        "the requested entry is recorded before the lock close"
    );
    Ok(())
}
