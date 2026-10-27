//! Tests for the role continuation: regenerating the asking role's real job
//! at the same attempt after a published grant, replaying into one queued
//! job, and resolving handlers through the registry.

use std::collections::BTreeMap;
use std::path::Path;

use tempfile::{TempDir, tempdir};

use super::{continuation_id, continue_role};
use crate::error::VivariumError;
use crate::letter::fixture::need_body_with;
use crate::letter::work::NeedFront;
use crate::pump::config::{PumpConfig, PumpTarget};
use crate::pump::envelope::correct;
use crate::pump::flow::def::{FlowDef, FlowKind};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::shipped;
use crate::pump::flow::index::{PayloadIdentity, RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::need_scope::{RulingDecision, ScopeRuling, ruling_note_body};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobSpec, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore, ScopeRequest as JournalRequest};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
/// The one configured pump target.
const TARGET: &str = "vivi";
/// The asking node: the build flow's planner.
const NODE: &str = "plan";
/// The step task of the asking node.
const TASK: &str = "t0001";
const THREAD: &str = "need thread: build the widget";
/// The path the test's published grant adds to the need's scope.
const GRANTED: &str = "src/lib.rs";

/// The pool job map of a test.
struct Jobs(BTreeMap<String, PoolJob>);

impl JobStore for Jobs {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.0
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.0
    }
}

struct Rig {
    dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
    run: RunRow,
    def: FlowDef,
}

/// The base job id of the asking chain: the planner's first attempt.
fn base() -> String {
    format!("step-{TASK}-a1")
}

/// A rig with a live build run, a waiting `plan` step, a need whose letter
/// delegates `src/pump/`, and one configured target.
fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let config = PumpConfig {
        targets: BTreeMap::from([(TARGET.to_string(), PumpTarget::default())]),
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), config);
    host.reply("need show", 0, THREAD);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let run = RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: NODE.into(),
        state: "live".into(),
    };
    store.insert_run(&run)?;
    store.insert_step(
        NEED,
        &StepRow {
            run: 1,
            node: NODE.into(),
            task: TASK.into(),
            state: StepState::Waiting,
            verdict: None,
        },
    )?;
    let mut mail = FakeMail::new("pump");
    let front = NeedFront {
        write_scope: vec![String::from("src/pump/")],
        ..NeedFront::new(vec![String::from("build the widget")])
    };
    let body = need_body_with(&front, "");
    let sent = mail.send_step("pump", "pump", "the need", &body, &[])?;
    if let Some(task) = mail.tasks.remove(&sent) {
        mail.tasks.insert(NEED.to_string(), task);
    }
    Ok(Rig {
        dir,
        ctx,
        store,
        mail,
        jobs: Jobs(BTreeMap::new()),
        run,
        def: load(FlowKind::Build)?,
    })
}

/// Seed the asking chain of a published grant: the planner's stale first
/// attempt, its envelope-fix correction (the job the corrected reply came
/// through), the step_jobs row of the attempt, and the journal row of the
/// request with its publication acknowledged.
fn a_published_ask(rig: &mut Rig) -> R {
    let base = base();
    let mut stale = JobSpec::new(
        base.clone(),
        "planner",
        "pump-planner",
        "Plan (n1 r1)",
        "planner brief: effective need write scope: delegated src/pump/",
    );
    stale.task = Some(TASK.into());
    rig.jobs.0.insert(base.clone(), PoolJob::from(stale));
    let mut fix = JobSpec::new(
        format!("{base}-c1"),
        correct::KIND,
        "pump-planner",
        "envelope fix: Plan",
        "Rewrite only the envelope; do not redo the work.",
    );
    fix.task = Some(TASK.into());
    rig.jobs.0.insert(format!("{base}-c1"), PoolJob::from(fix));
    rig.store.insert_step_job(
        NEED,
        &StepJobRow {
            job: base.clone(),
            run: 1,
            node: NODE.into(),
            task: TASK.into(),
            attempt: 1,
        },
    )?;
    rig.store.prepare_scope_request(&JournalRequest {
        source: base.clone(),
        need: NEED.into(),
        target: TARGET.into(),
        run: 1,
        node: NODE.into(),
        task: TASK.into(),
        attempt: 1,
        paths: vec![GRANTED.to_string()],
        reason: "the plan needs the file".into(),
        design: false,
        decision: RulingDecision::Grant,
        issue: None,
    })?;
    rig.store.acknowledge_scope_publication(&base, 1_000)?;
    Ok(())
}

/// Post the published grant on the need thread as the pump's reply role, so
/// the regenerated brief's effective scope carries it.
fn post_grant(rig: &mut Rig) -> R {
    let ruling = ScopeRuling {
        need: NEED.into(),
        target: TARGET.into(),
        run: 1,
        task: TASK.into(),
        paths: vec![GRANTED.to_string()],
        reason: "held by no open slice".into(),
        decision: RulingDecision::Grant,
        issue: None,
    };
    let body = ruling_note_body(&ruling, &base())?;
    rig.mail.foreign_reply(NEED, "mind", &body);
    Ok(())
}

/// Continue the plan node's role job through the shipped registry.
fn continue_plan(rig: &mut Rig, source: &str) -> Result<String, VivariumError> {
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &rig.def,
    };
    let registry = shipped::<Jobs, FakeMail>(&rig.ctx);
    continue_role(&cx, &mut rig.jobs, &registry, source)
}

/// The partial-acknowledgement fault: the pool took the job but the
/// step_jobs insert never landed.
fn forget_step_job(dir: &Path, job: &str) {
    let conn = rusqlite::Connection::open(dir.join(DB_FILENAME)).expect("fault connection");
    conn.execute("DELETE FROM step_jobs WHERE job = ?1", [job])
        .expect("fault delete");
}

#[test]
fn scope_continuation_replay_enqueues_once() -> R {
    let mut rig = rig()?;
    a_published_ask(&mut rig)?;
    post_grant(&mut rig)?;

    // The corrected envelope-fix reply is the source: the continuation keys
    // to its base and requeues the real role job.
    let id = continue_plan(&mut rig, &format!("{}-c1", base()))?;
    assert_eq!(id, continuation_id(&base()));
    let job = rig.jobs.0.get(&id).expect("continuation job queued");
    assert_eq!(
        job.kind, "planner",
        "the real role kind, never envelope-fix"
    );
    assert_eq!(job.role, "pump-planner");
    let identity = PayloadIdentity::of(&job.payload).map_err(VivariumError::Parse)?;
    assert_eq!(identity.need, NEED);
    assert_eq!(identity.run, 1);
    assert_eq!(identity.task, TASK);
    assert_eq!(identity.attempt, Some(1), "the attempt never moves");
    assert_eq!(job.payload["node"], NODE);

    let row = rig.store.step_job(&id)?.expect("continuation row");
    assert_eq!(row.attempt, 1);
    assert_eq!(row.node, NODE);
    assert_eq!(row.task, TASK);
    let step = rig.store.step_by_task(TASK)?.expect("step");
    assert_eq!(step.state, StepState::Running);
    assert_eq!(step.verdict, None);
    let journal = rig.store.scope_request(&base())?.expect("journal");
    assert_eq!(journal.continuation.as_deref(), Some(id.as_str()));

    // A replay of the same request queues nothing new.
    let replay = continue_plan(&mut rig, &base())?;
    assert_eq!(replay, id);
    assert_eq!(rig.jobs.0.len(), 3);

    // A crash after the pool took the job but before the index insert is
    // acknowledged: the reopened store replays into an adoption and one row.
    drop(rig.store);
    forget_step_job(rig.dir.path(), &id);
    rig.store = PumpStore::open(&rig.dir.path().join(DB_FILENAME))?;
    let resumed = continue_plan(&mut rig, &base())?;
    assert_eq!(resumed, id);
    assert_eq!(rig.jobs.0.len(), 3, "no duplicate job after the reopen");
    assert!(
        rig.store.step_job(&id)?.is_some(),
        "row acknowledged once more"
    );
    Ok(())
}

#[test]
fn scope_continuation_sees_the_widened_scope_in_a_fresh_brief() -> R {
    let mut rig = rig()?;
    a_published_ask(&mut rig)?;
    post_grant(&mut rig)?;
    let id = continue_plan(&mut rig, &base())?;
    let job = rig.jobs.0.get(&id).expect("continuation job");
    let body = job.body.as_str();
    assert!(
        body.contains("delegated src/pump/"),
        "delegation rendered: {body}"
    );
    assert!(
        body.contains("granted src/lib.rs"),
        "grant rendered: {body}"
    );
    assert!(
        body.contains(THREAD),
        "the fresh brief read the current thread"
    );

    // The stale source job is untouched: never copied, never rewritten.
    let stale = &rig.jobs.0[&base()];
    assert!(!stale.body.contains("granted"), "stale brief stays stale");
    assert_eq!(stale.kind, "planner");
    Ok(())
}

/// An override handler of the plan node: a distinct real job.
struct Override;

impl StepHandler for Override {
    fn start(&self, _cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let mut spec = JobSpec::new(
            format!("step-{}-a1", step.task),
            "override-kind",
            "override-role",
            "override subject",
            "override brief",
        );
        spec.task = Some(step.task.clone());
        Ok(StartAction::Job(Box::new(spec)))
    }
}

#[test]
fn scope_continuation_runs_through_the_registered_override() -> R {
    let mut rig = rig()?;
    a_published_ask(&mut rig)?;
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &rig.def,
    };
    let mut registry = shipped::<Jobs, FakeMail>(&rig.ctx);
    registry.node_handler(FlowKind::Build, NODE, Box::new(Override));
    let id = continue_role(&cx, &mut rig.jobs, &registry, &base())?;
    assert_eq!(id, continuation_id(&base()));
    let job = rig.jobs.0.get(&id).expect("override job queued");
    assert_eq!(job.kind, "override-kind");
    assert_eq!(job.role, "override-role");
    assert_eq!(
        job.payload["attempt"], 1,
        "the default id was replaced, not the attempt"
    );
    Ok(())
}

/// An override handler that opens and waits: no job to regenerate.
struct Opener;

impl StepHandler for Opener {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::OpenAndWait)
    }
}

#[test]
fn scope_continuation_requires_a_real_role_job() -> R {
    let mut rig = rig()?;
    a_published_ask(&mut rig)?;
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &rig.def,
    };
    let mut registry = shipped::<Jobs, FakeMail>(&rig.ctx);
    registry.node_handler(FlowKind::Build, NODE, Box::new(Opener));
    let err = continue_role(&cx, &mut rig.jobs, &registry, &base())
        .expect_err("an open-and-wait node has no job to regenerate");
    assert!(err.to_string().contains("no role job"), "{err}");
    assert!(
        rig.jobs.0.contains_key(&base()),
        "only the source jobs exist"
    );

    // A journal whose node the flow does not carry cannot continue, and the
    // refusal leaves nothing behind.
    rig.store.prepare_scope_request(&JournalRequest {
        source: "step-ghost-a1".into(),
        need: NEED.into(),
        target: TARGET.into(),
        run: 1,
        node: "ghost".into(),
        task: TASK.into(),
        attempt: 1,
        paths: vec![GRANTED.to_string()],
        reason: "unknown node".into(),
        design: false,
        decision: RulingDecision::Grant,
        issue: None,
    })?;
    let registry = shipped::<Jobs, FakeMail>(&rig.ctx);
    let err = continue_role(&cx, &mut rig.jobs, &registry, "step-ghost-a1")
        .expect_err("the flow has no ghost node");
    assert!(err.to_string().contains("ghost"), "{err}");
    Ok(())
}
