//! Tests for slice collection that re-slices: a timed-out hand goes to a split
//! job and a scope rework goes to a rework job.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::error::VivariumError;
use crate::mailspace::shipped::SLICER_ROLE;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::need_branch;
use crate::pump::flow::reslice;
use crate::pump::flow::scope::ScopeState;
use crate::pump::flow::slice_dispatch;
use crate::pump::flow::tick::{OPERATOR_HOLD, tick_with};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const BASE: &str = "2222222222222222222222222222222222222222";
const FAILED: &str = "+++\nanswer = \"failed\"\n+++\ngave up";
const DESIGN: &str = "+++\nanswer = \"blocked-scope\"\nscope_paths = [\"src/x.rs\"]\n\
                      scope_reason = 'needs a design'\nscope_design = true\n+++\nnothing changed";

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
    _host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
}

fn role(name: &str, kind: &str, pool: &str) -> RoutedRole {
    RoutedRole {
        name: name.into(),
        kind: Some(kind.into()),
        status: String::from("active"),
        pool: Some(pool.into()),
        read_only: None,
        model: None,
        models: Vec::new(),
    }
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let mut routing = (*ctx.routing).clone();
    routing.roles.retain(|r| r.name != SLICER_ROLE);
    routing
        .roles
        .push(role("hand", "hand", "test-planner-pool"));
    routing
        .roles
        .push(role(SLICER_ROLE, "slicer", "test-slicer-pool"));
    let ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    store.upsert_need_branch(&NeedBranchRow {
        need: NEED.into(),
        branch: need_branch::branch(NEED),
        base: Some(BASE.into()),
        status: need_branch::LIVE.into(),
        run: 1,
        target: "default".into(),
        priors: Vec::new(),
    })?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, "kind: build")?;
    Ok(Rig {
        _dir: dir,
        ctx,
        _host: host,
        store,
        mail,
        jobs: Jobs::default(),
    })
}

/// Send and register one running slice step with a write scope.
fn add_slice(rig: &mut Rig, id: &str, path: &str) -> Result<StepRow, VivariumError> {
    let task = rig.mail.send_step(
        "mind",
        "mind",
        &format!("slice {id}"),
        &format!("pump-step: {NEED} r1 slice:{id}\ntarget: default\n\ntitle {id}\n\nbrief {id}"),
        &[],
    )?;
    let slice = StepRow {
        run: 1,
        node: format!("slice:{id}"),
        task: task.clone(),
        state: StepState::Running,
        verdict: None,
    };
    rig.store.insert_step(NEED, &slice)?;
    rig.store
        .insert_slice_scope(&task, 1, &[path.to_string()])?;
    Ok(slice)
}

/// A started run with slice `a` (and an open slice `b`) whose hand job ended.
fn ended_slice(rig: &mut Rig, status: JobStatus, message: &str) -> Result<StepRow, VivariumError> {
    start(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("plan"))?;
    let slice = add_slice(rig, "a", "src/a.rs")?;
    add_slice(rig, "b", "src/b.rs")?;
    let id = slice_dispatch::dispatch(
        &rig.ctx,
        &mut rig.jobs,
        &rig.store,
        &rig.mail,
        NEED,
        &slice,
        1,
    )?;
    let state = if status == JobStatus::Timeout {
        "timeout"
    } else {
        "finished"
    };
    let receipt = format!(
        "pool result j1: {state}\npool p  slot s1  model m\nfrom main  default\n\
         push: none\nagent exit 0 in 12s, job 20s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n\n\n--- runner log tail\nlog line"
    );
    let job = rig
        .jobs
        .jobs
        .get_mut(&id)
        .ok_or_else(|| VivariumError::Message("no job".into()))?;
    job.status = status;
    job.outcome = Some(JobOutcome {
        state: state.into(),
        detail: String::new(),
        receipt,
        at: 0,
    });
    Ok(slice)
}

fn tick(rig: &mut Rig) -> R {
    tick_with(
        &rig.ctx,
        &mut rig.jobs,
        &rig.store,
        &mut rig.mail,
        &Registry::new(),
    )
}

fn jobs_of<'a>(rig: &'a Rig, kind: &str) -> Vec<&'a PoolJob> {
    rig.jobs
        .jobs
        .values()
        .filter(|job| job.kind == kind)
        .collect()
}

fn attempt(job: &PoolJob) -> Option<u64> {
    job.payload.get("attempt")?.as_u64()
}

#[test]
fn a_timed_out_hand_queues_one_split_job_and_no_second_attempt() -> R {
    let mut rig = rig()?;
    ended_slice(&mut rig, JobStatus::Timeout, "")?;

    tick(&mut rig)?;

    let resliced = jobs_of(&rig, reslice::KIND);
    assert_eq!(resliced.len(), 1, "{:?}", rig.jobs.jobs.keys());
    assert_eq!(resliced[0].payload["mode"], "split");
    assert!(resliced[0].body.contains("job: "), "{}", resliced[0].body);
    assert!(
        resliced[0].body.contains("budget: "),
        "{}",
        resliced[0].body
    );
    let hands = jobs_of(&rig, slice_dispatch::KIND);
    assert!(hands.iter().all(|job| attempt(job) == Some(1)));
    Ok(())
}

#[test]
fn a_scope_rework_queues_a_rework_job_listing_the_open_scopes() -> R {
    let mut rig = rig()?;
    let slice = ended_slice(&mut rig, JobStatus::Finished, DESIGN)?;

    tick(&mut rig)?;

    let resliced = jobs_of(&rig, reslice::KIND);
    assert_eq!(resliced.len(), 1, "{:?}", rig.jobs.jobs.keys());
    assert_eq!(resliced[0].payload["mode"], "rework");
    let body = &resliced[0].body;
    assert!(body.contains("src/x.rs"), "{body}");
    assert!(body.contains("needs a design"), "{body}");
    assert!(body.contains("src/b.rs"), "{body}");
    assert_eq!(
        rig.store.scope_state(&slice.task)?,
        Some(ScopeState::Rework)
    );
    Ok(())
}

#[test]
fn a_plain_failure_still_queues_the_second_attempt() -> R {
    let mut rig = rig()?;
    ended_slice(&mut rig, JobStatus::Finished, FAILED)?;

    tick(&mut rig)?;

    assert!(jobs_of(&rig, reslice::KIND).is_empty());
    let hands = jobs_of(&rig, slice_dispatch::KIND);
    assert!(hands.iter().any(|job| attempt(job) == Some(2)), "{hands:?}");
    Ok(())
}

/// A dispatched slice in scope-wait whose every holder is closed (it has none
/// recorded), held for the operator.
fn held_scope_wait(rig: &mut Rig) -> Result<StepRow, VivariumError> {
    start(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("plan"))?;
    let slice = add_slice(rig, "a", "src/a.rs")?;
    slice_dispatch::dispatch(
        &rig.ctx,
        &mut rig.jobs,
        &rig.store,
        &rig.mail,
        NEED,
        &slice,
        1,
    )?;
    rig.store.set_scope_state(&slice.task, ScopeState::Wait)?;
    rig.store
        .set_step_state(&slice.task, StepState::Waiting, Some(OPERATOR_HOLD))?;
    Ok(slice)
}

#[test]
fn a_held_scope_wait_slice_stays_held_until_the_hold_is_lifted() -> R {
    let mut rig = rig()?;
    let slice = held_scope_wait(&mut rig)?;
    let before = rig.jobs.jobs.len();

    tick(&mut rig)?;
    tick(&mut rig)?;

    let row = rig.store.step_by_task(&slice.task)?;
    let row = row.ok_or_else(|| VivariumError::Message("no step row".into()))?;
    assert_eq!(row.state, StepState::Waiting);
    assert_eq!(row.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.jobs.jobs.len(), before, "{:?}", rig.jobs.jobs.keys());
    assert_eq!(rig.store.scope_state(&slice.task)?, Some(ScopeState::Wait));

    rig.store
        .set_step_state(&slice.task, StepState::Waiting, None)?;
    tick(&mut rig)?;

    let row = rig.store.step_by_task(&slice.task)?;
    let row = row.ok_or_else(|| VivariumError::Message("no step row".into()))?;
    assert_eq!(row.state, StepState::Running);
    assert!(rig.jobs.jobs.len() > before, "{:?}", rig.jobs.jobs.keys());
    Ok(())
}
