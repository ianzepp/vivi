//! Tests for the `thread-change` phase, on a real mailspace: a reply by another
//! member after the run's start post restarts a live or parked run at `plan`,
//! raises once in a locked run, and a reply by the pump changes nothing.

use std::thread::sleep;
use std::time::Duration;

use super::{THREAD_CHANGED, THREAD_CHECK_FAILED, ThreadChange};
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::handlers::{Registry, TickPhase};
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, MailspacePort};
use crate::pump::host::Ctx;
use crate::pump::store::{DB_FILENAME, PumpStore};
use vivi_error::VivariumError;
use vivi_mail::letter::fixture::{record_body, step_body};

type R = Result<(), VivariumError>;

const PUMP: &str = "mind";

struct Rig {
    _project: Project,
    ctx: Ctx,
    store: PumpStore,
    port: MailspacePort,
    need: String,
}

/// A need with an open pump record and a build run started at `plan`.
fn rig() -> Result<Rig, VivariumError> {
    let p = project();
    let ctx = p.pump_ctx();
    let need = p.need("change me");
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut port = MailspacePort {
        mailspace: crate::pump::record::PumpMailspace::new(p.mailspace.clone()),
        pump_identity: PUMP.into(),
    };
    MailWriter::open_record(&mut port, &need, &record_body(&need, "build"))?;
    start(&ctx, &store, &mut port, &need, Some("plan"))?;
    Ok(Rig {
        _project: p,
        ctx,
        store,
        port,
        need,
    })
}

impl Rig {
    /// A reply on the need's thread, one second after the run's start post.
    fn reply_as(&self, from: &str) -> R {
        sleep(Duration::from_millis(1100));
        let to = if from == PUMP { "operator" } else { PUMP };
        self.port.mailspace.lock().reply(
            &self.need,
            from,
            vec![to.into()],
            Vec::new(),
            None,
            "actually, do it differently".into(),
        )?;
        Ok(())
    }

    fn phase(&mut self) -> R {
        ThreadChange.run(&self.ctx, &mut (), &self.store, &mut self.port)
    }

    fn runs(&self) -> Result<Vec<(u32, String, String)>, VivariumError> {
        Ok(self
            .store
            .runs_of(&self.need)?
            .into_iter()
            .map(|run| (run.run, run.entry, run.state))
            .collect())
    }

    /// A waiting step row for `node` on run 1, over a real task.
    fn hold(&mut self, node: &str) -> R {
        let task =
            self.port
                .send_step(PUMP, PUMP, node, &step_body(&self.need, 1, node, ""), &[])?;
        self.store.insert_step(
            &self.need,
            &StepRow {
                run: 1,
                node: node.into(),
                task,
                state: StepState::Waiting,
                verdict: None,
            },
        )
    }

    fn exceptions(&self) -> String {
        std::fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }
}

#[test]
fn a_reply_by_another_member_restarts_a_live_run_at_plan() -> R {
    let mut rig = rig()?;
    rig.reply_as("operator")?;
    rig.phase()?;
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0], (1, "plan".into(), "superseded".into()));
    assert_eq!(runs[1], (2, "plan".into(), "live".into()));
    assert_eq!(rig.exceptions(), "");
    // The next pass sees the reply as older than run 2's start: no third run.
    rig.phase()?;
    assert_eq!(rig.runs()?.len(), 2);
    Ok(())
}

#[test]
fn the_run_started_post_is_stored_as_written_and_still_counts() -> R {
    let mut rig = rig()?;
    let thread = rig
        .port
        .mailspace
        .lock()
        .thread(&rig.need, false, usize::MAX, usize::MAX)?;
    let start = thread
        .iter()
        .find(|m| vivi_mail::letter::prose_of(&m.body).starts_with("pump: run r1 started"))
        .expect("the run-started post");
    assert!(!start.body.contains("+++"), "{}", start.body);
    rig.reply_as("operator")?;
    rig.phase()?;
    assert_eq!(rig.runs()?.len(), 2);
    Ok(())
}

#[test]
fn a_reply_by_the_pump_identity_changes_nothing() -> R {
    let mut rig = rig()?;
    rig.reply_as(PUMP)?;
    rig.phase()?;
    assert_eq!(rig.runs()?, [(1, "plan".into(), "live".into())]);
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_parked_run_restarts_at_plan() -> R {
    let mut rig = rig()?;
    rig.hold("parked")?;
    rig.reply_as("operator")?;
    rig.phase()?;
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[1], (2, "plan".into(), "live".into()));
    Ok(())
}

#[test]
fn a_reply_in_a_locked_run_raises_once_and_starts_nothing() -> R {
    let mut rig = rig()?;
    rig.hold("locked")?;
    rig.reply_as("operator")?;
    rig.phase()?;
    rig.phase()?;
    assert_eq!(rig.runs()?, [(1, "plan".into(), "live".into())]);
    let raised = rig.exceptions();
    assert_eq!(raised.matches(THREAD_CHANGED).count(), 1, "{raised}");
    assert!(raised.contains(&rig.need), "{raised}");
    Ok(())
}

#[test]
fn a_reply_after_merge_main_started_raises_and_does_not_restart() -> R {
    let mut rig = rig()?;
    rig.hold("merge")?;
    rig.reply_as("operator")?;
    rig.phase()?;
    assert_eq!(rig.runs()?, [(1, "plan".into(), "live".into())]);
    assert_eq!(rig.exceptions().matches(THREAD_CHANGED).count(), 1);
    Ok(())
}

/// Corrupt the stored date of every letter the pump sent, the run-start post
/// included.
fn corrupt_pump_dates(rig: &Rig) {
    let db = rig.ctx.project.join(".vivi").join("mail.sqlite");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let changed = conn
        .execute(
            "UPDATE message_metadata SET date = 'last tuesday' WHERE from_addr LIKE '%mind%'",
            [],
        )
        .unwrap();
    assert!(changed > 0, "no pump letter to corrupt");
}

#[test]
fn an_unparsable_start_date_raises_once_and_restarts_nothing() -> R {
    let mut rig = rig()?;
    rig.reply_as("operator")?;
    corrupt_pump_dates(&rig);
    rig.phase()?;
    rig.phase()?;
    assert_eq!(rig.runs()?, [(1, "plan".into(), "live".into())]);
    let raised = rig.exceptions();
    assert_eq!(raised.matches(THREAD_CHECK_FAILED).count(), 1, "{raised}");
    assert!(raised.contains("last tuesday"), "{raised}");
    assert!(raised.contains(&rig.need), "{raised}");
    assert_eq!(raised.matches(THREAD_CHANGED).count(), 0, "{raised}");
    Ok(())
}

#[test]
fn a_valid_run_raises_no_check_failure() -> R {
    let mut rig = rig()?;
    rig.phase()?;
    rig.phase()?;
    assert_eq!(rig.runs()?, [(1, "plan".into(), "live".into())]);
    assert_eq!(rig.exceptions().matches(THREAD_CHECK_FAILED).count(), 0);
    Ok(())
}

impl Rig {
    /// Make `need` depend on a fresh open prerequisite need, so its build is
    /// not admitted; returns the prerequisite's handle.
    fn block_on(&self, need: &str) -> Result<String, VivariumError> {
        let dep = self._project.need("blocks the change");
        self.port.mailspace.lock().set_dependencies(
            "mind",
            need,
            "need",
            std::slice::from_ref(&dep),
            &[],
        )?;
        Ok(dep)
    }

    /// Drop the prerequisite edge again, so admission is allowed.
    fn unblock(&self, need: &str, dep: &str) -> R {
        self.port
            .mailspace
            .lock()
            .set_dependencies("mind", need, "need", &[], std::slice::from_ref(&dep))?;
        Ok(())
    }

    /// One intake-and-restarts pass: all a deferred restart needs to finish.
    fn pass(&mut self) -> R {
        let mut flow: Registry<(), MailspacePort> = Registry::new();
        crate::pump::flow::intake::register(&mut flow);
        crate::pump::flow::advance::restart::register(&mut flow);
        for phase in flow.phases() {
            phase.run(&self.ctx, &mut (), &self.store, &mut self.port)?;
        }
        Ok(())
    }
}

#[test]
fn a_blocked_thread_change_keeps_plan_and_needs_no_second_reply() -> R {
    let mut rig = rig()?;
    let need = rig.need.clone();
    let dep = rig.block_on(&need)?;
    rig.reply_as("operator")?;

    rig.phase()?;

    // The change is accepted: the run waits `restarting` at `plan` with its
    // old sentinel still open, and no successor exists yet.
    let runs = rig.runs()?;
    assert_eq!(runs, [(1, "plan".into(), "restarting".into())], "{runs:?}");
    assert_eq!(
        rig.store.restart_entry(&need, 1)?.as_deref(),
        Some("plan"),
        "the requested entry is durable"
    );
    assert_eq!(rig.store.build_waits()?.len(), 1, "the blocker is queued");
    assert_eq!(rig.port.sentinel_count(&need)?, 1, "no successor yet");
    assert_eq!(rig.exceptions(), "");

    // The reply needs no follow-up: the phase watches live runs only and the
    // saved entry survives a reopened store.
    rig.phase()?;
    let reopened = PumpStore::open(&rig.ctx.home.join(DB_FILENAME))?;
    assert_eq!(
        reopened.restart_entry(&need, 1)?.as_deref(),
        Some("plan")
    );

    // The first eligible admission yields exactly one successor at `plan`.
    rig.unblock(&need, &dep)?;
    rig.pass()?;
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 2, "exactly one successor: {runs:?}");
    assert_eq!(
        (runs[0].state.as_str(), runs[1].state.as_str()),
        ("superseded", "live")
    );
    assert_eq!(runs[1].entry, "plan");
    assert!(rig.store.build_waits()?.is_empty());

    // No repeated action: another pass and another thread check add nothing.
    rig.pass()?;
    rig.phase()?;
    assert_eq!(rig.runs()?.len(), 2);
    assert_eq!(rig.exceptions(), "");
    Ok(())
}
