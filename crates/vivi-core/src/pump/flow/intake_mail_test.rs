//! Intake over the real mailspace: a waiting build holds no live-run slot, a
//! held prerequisite that lands releases its waiter with no operator action, a
//! start left `starting` recovers even when a dependency arrived afterwards,
//! and a prerequisite read error is isolated to its own candidate.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::register;
use crate::mailspace::SendRequest;
use crate::mailspace::pump_note::Note;
use crate::pump::fixture::{Project, T0, project};
use crate::pump::flow::advance::restart::RESTART_STUCK;
use crate::pump::flow::build_prereq::{PREREQ_HELD, PREREQ_UNREADABLE};
use crate::pump::flow::fanout::SliceEdges;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{FLOW_VERSION_NUMBER, RunRow, StepRow, StepState};
use crate::pump::flow::mail::{
    MailReader, MailWriter, MailspacePort, NeedStatus, OpenRecords, TaskState,
};
use crate::pump::flow::slice_dispatch::SliceMail;
use crate::pump::flow::tick::tick_with;
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use vivi_error::VivariumError;
use vivi_mail::letter::fixture::{need_body_with, record_body};
use vivi_mail::letter::records::NoteWord;
use vivi_mail::letter::work::{NeedFront, StepFront};

type R = Result<(), VivariumError>;

/// The pump identity of the default config's `reply_from`.
const PUMP: &str = "mind";

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

/// Send a need from `operator` to the pump identity, depending on `deps`.
fn send_need(project: &Project, subject: &str, deps: &[&str]) -> String {
    let front = NeedFront {
        depends_on: deps.iter().map(ToString::to_string).collect(),
        ..NeedFront::new(vec![String::from("it works")])
    };
    project
        .mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec![PUMP.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: need_body_with(&front, ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

fn open_record(port: &mut MailspacePort, need: &str) -> R {
    MailWriter::open_record(port, need, &record_body(need, "build")).map(drop)
}

fn runs(store: &PumpStore, need: &str) -> Result<usize, VivariumError> {
    Ok(store.runs_of(need)?.len())
}

fn held(store: &PumpStore, kind: &str) -> Result<usize, VivariumError> {
    Ok(store
        .exceptions()?
        .into_iter()
        .filter(|(_, found, _, _)| found == kind)
        .count())
}

#[test]
fn waiting_build_uses_no_live_run_slot() -> R {
    let project = project();
    let ctx = project.pump_ctx();
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut port = MailspacePort {
        mailspace: crate::pump::record::PumpMailspace::new(project.mailspace.clone()),
        pump_identity: PUMP.into(),
    };
    let mut jobs = Jobs::default();
    let mut registry = Registry::new();
    register(&mut registry);
    let mut ctx = ctx;
    Arc::make_mut(&mut ctx.cfg).flow.max_live_runs = Some(1);

    let prereq = send_need(&project, "prerequisite", &[]);
    let waiting = send_need(&project, "high priority", &[&prereq]);
    let eligible = send_need(&project, "low priority", &[]);
    open_record(&mut port, &waiting)?;
    open_record(&mut port, &eligible)?;
    store.set_need_priority(&waiting, 9, 1)?;
    store.set_need_priority(&eligible, 0, 1)?;

    tick_with(&ctx, &mut jobs, &store, &mut port, &registry)?;

    // The eligible need took the one slot; the blocked build took nothing.
    assert_eq!(runs(&store, &eligible)?, 1);
    assert_eq!(runs(&store, &waiting)?, 0);
    assert_eq!(port.sentinel_count(&waiting)?, 0);
    assert!(port.bound_units(&waiting)?.is_empty());
    assert!(jobs.jobs.is_empty());
    // The wait was assessed even though the cap was full.
    assert_eq!(store.build_waits()?.len(), 1);

    // A deferred build restart, a higher-priority waiter that lands, and
    // another ready need: all three compete for the freed slots by rank.
    let parked = send_need(&project, "deferred restart", &[]);
    open_record(&mut port, &parked)?;
    store.insert_run(&RunRow {
        need: parked.clone(),
        run: 1,
        flow: "build".into(),
        flow_version: FLOW_VERSION_NUMBER,
        kind: "build".into(),
        entry: "plan".into(),
        state: "restarting".into(),
    })?;
    store.set_restart_entry(&parked, 1, "rework1")?;
    store.set_need_priority(&parked, 5, 1)?;
    let later = send_need(&project, "later ready", &[]);
    open_record(&mut port, &later)?;
    store.set_need_priority(&later, 1, 1)?;

    // The prerequisite lands; the waiting build now outranks the restart.
    MailWriter::close_need(&mut port, &prereq, "landed")?;
    Arc::make_mut(&mut ctx.cfg).flow.max_live_runs = Some(2);
    tick_with(&ctx, &mut jobs, &store, &mut port, &registry)?;
    assert_eq!(runs(&store, &waiting)?, 1, "the higher rank took the slot");
    assert_eq!(
        runs(&store, &parked)?,
        1,
        "the deferred restart stays queued"
    );
    assert_eq!(
        store.restart_entry(&parked, 1)?.as_deref(),
        Some("rework1"),
        "the requested entry stays durable"
    );
    assert!(store.build_waits()?.is_empty());

    // A reopened store still knows the queued restart.
    let reopened = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    assert_eq!(
        reopened.restart_entry(&parked, 1)?.as_deref(),
        Some("rework1")
    );

    // One more slot, past the stuck interval: the restart starts its one
    // successor at its saved entry, and the parent takes no slot of its own.
    project.host.set_now(T0 + 700);
    Arc::make_mut(&mut ctx.cfg).flow.max_live_runs = Some(3);
    tick_with(&ctx, &mut jobs, &store, &mut port, &registry)?;
    let rows = store.runs_of(&parked)?;
    assert_eq!(rows.len(), 2, "exactly one successor");
    assert_eq!(
        (rows[1].run, rows[1].entry.as_str(), rows[1].state.as_str()),
        (2, "rework1", "live")
    );
    assert_eq!(rows[0].state, "superseded");
    assert_eq!(
        runs(&store, &later)?,
        0,
        "parent and successor count once: the cap is full"
    );
    assert_eq!(held(&store, RESTART_STUCK)?, 0, "queued is not stuck");
    Ok(())
}

#[test]
fn a_held_prerequisite_lands_and_releases_its_waiter() -> R {
    let project = project();
    let ctx = project.pump_ctx();
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut port = MailspacePort {
        mailspace: crate::pump::record::PumpMailspace::new(project.mailspace.clone()),
        pump_identity: PUMP.into(),
    };
    let mut jobs = Jobs::default();
    let mut registry = Registry::new();
    register(&mut registry);

    let prereq = send_need(&project, "prerequisite", &[]);
    let waiting = send_need(&project, "dependent", &[&prereq]);
    open_record(&mut port, &waiting)?;
    // The prerequisite is done and moving, held at an operator-only node.
    store.insert_run(&RunRow {
        need: prereq.clone(),
        run: 1,
        flow: "build".into(),
        flow_version: FLOW_VERSION_NUMBER,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    })?;
    store.insert_step(
        &prereq,
        &StepRow {
            run: 1,
            node: "locked".into(),
            task: String::from("t-locked"),
            state: StepState::Waiting,
            verdict: None,
        },
    )?;

    tick_with(&ctx, &mut jobs, &store, &mut port, &registry)?;
    assert_eq!(runs(&store, &waiting)?, 0);
    assert_eq!(held(&store, PREREQ_HELD)?, 1);

    // The operator lands the prerequisite: no letter, no operator action on
    // the dependent; the next tick starts it.
    MailWriter::close_need(&mut port, &prereq, "landed")?;
    tick_with(&ctx, &mut jobs, &store, &mut port, &registry)?;
    assert_eq!(runs(&store, &waiting)?, 1);
    assert!(store.build_waits()?.is_empty());
    Ok(())
}

#[test]
fn a_started_run_recovers_even_when_a_dependency_arrived_after_it_started() -> R {
    let project = project();
    let ctx = project.pump_ctx();
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut port = MailspacePort {
        mailspace: crate::pump::record::PumpMailspace::new(project.mailspace.clone()),
        pump_identity: PUMP.into(),
    };
    let mut jobs = Jobs::default();
    let mut registry = Registry::new();
    register(&mut registry);

    let prereq = send_need(&project, "prerequisite", &[]);
    let build = send_need(&project, "dependent", &[&prereq]);
    open_record(&mut port, &build)?;
    // A run that crossed admission before the dependency existed.
    store.insert_run(&RunRow {
        need: build.clone(),
        run: 1,
        flow: "build".into(),
        flow_version: FLOW_VERSION_NUMBER,
        kind: "build".into(),
        entry: "plan".into(),
        state: "starting".into(),
    })?;

    tick_with(&ctx, &mut jobs, &store, &mut port, &registry)?;
    let rows = store.runs_of(&build)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, "live");
    assert!(store.build_waits()?.is_empty());
    Ok(())
}

/// A reader whose prerequisite read fails for one need.
struct Silenced {
    inner: MailspacePort,
    need: String,
}

impl MailWriter for Silenced {
    fn send_step(
        &mut self,
        from: &str,
        to: &str,
        subject: &str,
        body: &str,
        depends_on: &[String],
    ) -> Result<String, VivariumError> {
        self.inner.send_step(from, to, subject, body, depends_on)
    }
    fn bind(&mut self, need: &str, units: &[String]) -> Result<(), VivariumError> {
        self.inner.bind(need, units)
    }
    fn reply(&mut self, handle: &str, body: &str) -> Result<(), VivariumError> {
        self.inner.reply(handle, body)
    }
    fn post_to_need_audience(&mut self, need: &str, body: &str) -> Result<(), VivariumError> {
        self.inner.post_to_need_audience(need, body)
    }
    fn close_task(
        &mut self,
        handle: &str,
        verdict: &str,
        branch: Option<&str>,
        commit: Option<&str>,
    ) -> Result<(), VivariumError> {
        self.inner.close_task(handle, verdict, branch, commit)
    }
    fn reopen_task(&mut self, handle: &str) -> Result<(), VivariumError> {
        self.inner.reopen_task(handle)
    }
    fn close_need(&mut self, need: &str, note: &str) -> Result<(), VivariumError> {
        self.inner.close_need(need, note)
    }
    fn open_record(&mut self, need: &str, body: &str) -> Result<String, VivariumError> {
        MailWriter::open_record(&mut self.inner, need, body)
    }
    fn close_record(&mut self, need: &str, outcome: &str) -> Result<(), VivariumError> {
        self.inner.close_record(need, outcome)
    }
}

impl MailReader for Silenced {
    fn task_state(&self, handle: &str) -> Result<TaskState, VivariumError> {
        self.inner.task_state(handle)
    }
    fn need_state(&self, need: &str) -> Result<NeedStatus, VivariumError> {
        self.inner.need_state(need)
    }
    fn need_marks(&self, need: &str) -> Result<Vec<String>, VivariumError> {
        self.inner.need_marks(need)
    }
    fn open_record(&self, need: &str) -> Result<Option<(String, String)>, VivariumError> {
        MailReader::open_record(&self.inner, need)
    }
    fn bound_units(&self, need: &str) -> Result<Vec<String>, VivariumError> {
        self.inner.bound_units(need)
    }
    fn sentinel_count(&self, need: &str) -> Result<usize, VivariumError> {
        self.inner.sentinel_count(need)
    }
    fn need_prerequisites(&self, need: &str) -> Result<Vec<String>, VivariumError> {
        if need == self.need {
            return Err(VivariumError::Message(String::from("graph unreadable")));
        }
        self.inner.need_prerequisites(need)
    }
    fn last_closed_record(&self, need: &str) -> Result<Option<String>, VivariumError> {
        self.inner.last_closed_record(need)
    }
    fn sent_step(&self, subject: &str, step: &StepFront) -> Result<Option<String>, VivariumError> {
        self.inner.sent_step(subject, step)
    }
}

impl OpenRecords for Silenced {
    fn open_record_needs(&self) -> Result<Vec<(String, String)>, VivariumError> {
        self.inner.open_record_needs()
    }
    fn unreadable_records(&self) -> Result<Vec<(String, String)>, VivariumError> {
        self.inner.unreadable_records()
    }
    fn need_letter_error(&self, need: &str) -> Result<Option<String>, VivariumError> {
        self.inner.need_letter_error(need)
    }
    fn unreadable_actions(&self) -> Result<Vec<(String, String)>, VivariumError> {
        self.inner.unreadable_actions()
    }
}

impl SliceMail for Silenced {
    fn task_body(&self, task: &str) -> Result<String, VivariumError> {
        self.inner.task_body(task)
    }
    fn marked_replies(
        &self,
        task: &str,
        marker: &str,
    ) -> Result<Vec<(String, String)>, VivariumError> {
        self.inner.marked_replies(task, marker)
    }
    fn notes(&self, task: &str, word: NoteWord) -> Result<Vec<(String, Note)>, VivariumError> {
        self.inner.notes(task, word)
    }
    fn notes_from(
        &self,
        task: &str,
        word: NoteWord,
        sender: &str,
    ) -> Result<Vec<(String, Note)>, VivariumError> {
        self.inner.notes_from(task, word, sender)
    }
    fn resolve_want(&self, id: &str) -> Result<Option<String>, VivariumError> {
        self.inner.resolve_want(id)
    }
}

impl SliceEdges for Silenced {
    fn add_dependency(&mut self, task: &str, depends_on: &str) -> Result<(), VivariumError> {
        self.inner.add_dependency(task, depends_on)
    }
}

#[test]
fn a_prerequisite_read_error_is_isolated_to_its_own_candidate() -> R {
    let project = project();
    let ctx = project.pump_ctx();
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut inner = MailspacePort {
        mailspace: crate::pump::record::PumpMailspace::new(project.mailspace.clone()),
        pump_identity: PUMP.into(),
    };
    let mut jobs = Jobs::default();
    let mut registry = Registry::new();
    register(&mut registry);

    let unreadable = send_need(&project, "unreadable candidate", &[]);
    let sound = send_need(&project, "sound candidate", &[]);
    open_record(&mut inner, &unreadable)?;
    open_record(&mut inner, &sound)?;
    let mut mail = Silenced {
        inner,
        need: unreadable.clone(),
    };

    tick_with(&ctx, &mut jobs, &store, &mut mail, &registry)?;

    // The unreadable candidate waits; the unrelated need is still admitted.
    assert_eq!(runs(&store, &unreadable)?, 0);
    assert_eq!(runs(&store, &sound)?, 1);
    assert_eq!(held(&store, PREREQ_UNREADABLE)?, 1);
    Ok(())
}

#[test]
fn a_repair_need_with_an_open_prerequisite_still_starts() -> R {
    let project = project();
    let ctx = project.pump_ctx();
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut port = MailspacePort {
        mailspace: crate::pump::record::PumpMailspace::new(project.mailspace.clone()),
        pump_identity: PUMP.into(),
    };
    let mut jobs = Jobs::default();
    let mut registry = Registry::new();
    register(&mut registry);

    let prereq = send_need(&project, "prerequisite", &[]);
    let repair = send_need(&project, "repair", &[&prereq]);
    MailWriter::open_record(
        &mut port,
        &repair,
        &vivi_mail::letter::fixture::repair_record_body(&repair, "default", None, ""),
    )?;

    tick_with(&ctx, &mut jobs, &store, &mut port, &registry)?;
    assert_eq!(runs(&store, &repair)?, 1);
    assert!(store.build_waits()?.is_empty());
    Ok(())
}
