//! Tests for the `thread-change` phase, on a real mailspace: a reply by another
//! member after the run's start post restarts a live or parked run at `plan`,
//! raises once in a locked run, and a reply by the pump changes nothing.

use std::collections::BTreeMap;
use std::thread::sleep;
use std::time::Duration;

use super::{THREAD_CHANGED, THREAD_CHECK_FAILED, ThreadChange};
use crate::mailspace::SendRequest;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::handlers::{Registry, TickPhase};
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, MailspacePort};
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use vivi_error::VivariumError;
use vivi_mail::letter::fixture::{need_body_with, record_body, step_body};
use vivi_mail::letter::work::NeedFront;

type R = Result<(), VivariumError>;

const PUMP: &str = "mind";

struct Rig {
    _project: Project,
    ctx: Ctx,
    store: PumpStore,
    port: MailspacePort,
    need: String,
}

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

/// A need with an open pump record and a build run started at `plan`.
fn rig() -> Result<Rig, VivariumError> {
    let p = project();
    let need = p.need("change me");
    rig_for(p, need)
}

/// A need that depends on `deps`, with the same open record and run.
fn rig_with_deps(deps: &[&str]) -> Result<Rig, VivariumError> {
    let p = project();
    let handles: Vec<String> = deps.iter().map(|subject| p.need(subject)).collect();
    let front = NeedFront {
        depends_on: handles,
        ..NeedFront::new(vec![String::from("it works")])
    };
    let need = p
        .mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec![PUMP.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "change me".into(),
            body: need_body_with(&front, ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .map_err(|error| VivariumError::Other(error.to_string()))?
        .delivered
        .remove(0)
        .handle;
    rig_for(p, need)
}

fn rig_for(p: Project, need: String) -> Result<Rig, VivariumError> {
    let ctx = p.pump_ctx();
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

    fn sentinel(&self, run: u32) -> Result<String, VivariumError> {
        self.store
            .steps_of(&self.need, run)?
            .into_iter()
            .find(|row| row.node == "sentinel")
            .map(|row| row.task)
            .ok_or_else(|| VivariumError::Other(format!("no sentinel in r{run}")))
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

#[test]
fn a_parked_thread_change_keeps_plan_and_needs_no_second_reply() -> R {
    let mut rig = rig_with_deps(&["prerequisite"])?;
    let dep = rig.port.need_prerequisites(&rig.need)?.remove(0);
    let old_sentinel = rig.sentinel(1)?;
    rig.reply_as("operator")?;

    rig.phase()?;

    assert_eq!(rig.runs()?, [(1, "plan".into(), "restarting".into())]);
    assert_eq!(
        rig.store.restart_entry(&rig.need, 1)?.as_deref(),
        Some("plan"),
        "the accepted change keeps one requested entry"
    );
    assert_eq!(
        rig.port.task_state(&old_sentinel)?.status,
        crate::pump::flow::mail::TaskStatus::Open,
        "the old sentinel waits for the new binding"
    );
    assert_eq!(rig.store.build_waits()?.len(), 1, "the blocker is queued");
    assert_eq!(rig.exceptions(), "");

    // No second reply and no second phase pass starts anything while blocked.
    rig.phase()?;
    assert_eq!(rig.runs()?, [(1, "plan".into(), "restarting".into())]);

    let reopened = PumpStore::open(&rig.ctx.home.join(DB_FILENAME))?;
    assert_eq!(
        reopened.restart_entry(&rig.need, 1)?.as_deref(),
        Some("plan"),
        "the requested entry survives a reopened store"
    );

    // The prerequisite lands: the first eligible admission starts exactly one
    // successor at the saved entry, with no second operator reply.
    MailWriter::close_need(&mut rig.port, &dep, "landed")?;
    let mut jobs = Jobs::default();
    let mut registry = Registry::new();
    crate::pump::flow::intake::register(&mut registry);
    crate::pump::flow::advance::restart::register(&mut registry);
    crate::pump::flow::tick::tick_with(&rig.ctx, &mut jobs, &reopened, &mut rig.port, &registry)?;

    assert_eq!(
        rig.runs()?,
        [
            (1, "plan".into(), "superseded".into()),
            (2, "plan".into(), "live".into())
        ],
        "exactly one successor at plan"
    );
    let new_sentinel = rig.sentinel(2)?;
    assert_eq!(
        rig.port.task_state(&old_sentinel)?.verdict.as_deref(),
        Some("superseded")
    );
    assert_eq!(
        rig.port.task_state(&new_sentinel)?.status,
        crate::pump::flow::mail::TaskStatus::Open
    );
    assert!(rig.store.build_waits()?.is_empty());

    rig.phase()?;
    assert_eq!(rig.runs()?.len(), 2, "the old reply never restarts again");
    Ok(())
}
