//! Tests for slice collection: the built-slice hooks run in registration order.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{BuiltHook, register_defaults, runner_env_failure};
use crate::error::VivariumError;
use crate::letter::fixture::task_body_with;
use crate::letter::work::TaskFront;
use crate::pump::config::PumpConfig;
use crate::pump::envelope::roles::Hand;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::collect_step::RunCx;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailWriter, TaskStatus};
use crate::pump::flow::need_branch;
use crate::pump::flow::slice_dispatch::{self, DEVIATIONS_MARKER};
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::rung_result::EnvKind;
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const BASE: &str = "2222222222222222222222222222222222222222";
const BUILT: &str = "+++\nanswer = \"built\"\ndeviations = ['changed a signature']\n+++\ndone";

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

/// A hook that replies with its name on the built slice's task.
struct Tag(&'static str);

impl BuiltHook<Jobs, FakeMail> for Tag {
    fn built(&self, cx: &mut RunCx<'_, Jobs, FakeMail>, step: &StepRow, _envelope: &Hand) -> R {
        cx.mail.reply(&step.task, self.0).map(|_| ())
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

/// A live need-branch row on the slices' target, so hands have a start point.
fn seed_need_branch(store: &PumpStore) -> R {
    store.upsert_need_branch(&NeedBranchRow {
        need: NEED.into(),
        branch: need_branch::branch(NEED),
        base: Some(BASE.into()),
        status: need_branch::LIVE.into(),
        run: 1,
        target: "default".into(),
        priors: Vec::new(),
    })
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("hand"),
        kind: Some(String::from("hand")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: None,
        model: None,
        models: Vec::new(),
    });
    let ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    seed_need_branch(&store)?;
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

/// A started run with one dispatched slice whose hand job finished `built`.
fn built_slice(rig: &mut Rig) -> Result<StepRow, VivariumError> {
    start(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("plan"))?;
    let task = rig.mail.send_step(
        "mind",
        "mind",
        "slice a",
        &task_body_with(
            &TaskFront {
                target: Some("default".into()),
                ..TaskFront::step(NEED, 1, "slice:a")
            },
            "title a\n\nbrief a",
        ),
        &[],
    )?;
    let slice = StepRow {
        run: 1,
        node: "slice:a".into(),
        task: task.clone(),
        state: StepState::Pending,
        verdict: None,
    };
    rig.store.insert_step(NEED, &slice)?;
    rig.store
        .insert_slice_scope(&task, 1, &["src/a.rs".to_string()])?;
    let id = slice_dispatch::dispatch(
        &rig.ctx,
        &mut rig.jobs,
        &rig.store,
        &rig.mail,
        NEED,
        &slice,
        1,
    )?;
    let receipt = format!(
        "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
         push: pushed vivi/unit/x-a1 abcdef0123\nagent exit 0 in 12s, job 20s, started from 1234567890\n\n\
         --- receipt\n{BUILT}\n\n--- stderr tail\n\n\n--- runner log tail\nlog line"
    );
    let job = rig
        .jobs
        .jobs
        .get_mut(&id)
        .ok_or_else(|| VivariumError::Message("no job".into()))?;
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt,
        at: 0,
    });
    Ok(slice)
}

#[test]
fn built_hooks_run_in_registration_order_after_the_default_and_the_slice_stays_open() -> R {
    let mut rig = rig()?;
    let slice = built_slice(&mut rig)?;
    let mut registry = Registry::<Jobs, FakeMail>::new();
    register_defaults(&mut registry);
    registry.built_hook(Box::new(Tag("hook-one")));
    registry.built_hook(Box::new(Tag("hook-two")));

    tick_with(
        &rig.ctx,
        &mut rig.jobs,
        &rig.store,
        &mut rig.mail,
        &registry,
    )?;

    let replies = &rig.mail.tasks[&slice.task].replies;
    let position = |needle: &str| replies.iter().position(|r| r.contains(needle));
    let deviations = position(DEVIATIONS_MARKER);
    let one = position("hook-one");
    let two = position("hook-two");
    assert!(
        deviations.is_some() && one.is_some() && two.is_some(),
        "{replies:?}"
    );
    assert!(deviations < one && one < two, "{replies:?}");
    assert_eq!(rig.mail.tasks[&slice.task].state.status, TaskStatus::Open);
    Ok(())
}

#[test]
fn a_registry_built_by_hand_still_posts_the_default_notes() -> R {
    let mut rig = rig()?;
    let slice = built_slice(&mut rig)?;

    tick_with(
        &rig.ctx,
        &mut rig.jobs,
        &rig.store,
        &mut rig.mail,
        &Registry::new(),
    )?;

    let replies = &rig.mail.tasks[&slice.task].replies;
    assert!(replies.iter().any(|r| r.contains("changed a signature")));
    Ok(())
}

/// A cx over the rig's run 1, for calling collection helpers directly.
fn with_cx<T>(
    rig: &mut Rig,
    body: impl FnOnce(&mut RunCx<'_, Jobs, FakeMail>) -> Result<T, VivariumError>,
) -> Result<T, VivariumError> {
    let run = rig.store.runs_of(NEED)?.remove(0);
    let registry = Registry::<Jobs, FakeMail>::new();
    let decide = |_: &_, _: &_| Err(VivariumError::Message("no decision".into()));
    let mut cx = RunCx {
        ctx: &rig.ctx,
        st: &mut rig.jobs,
        store: &rig.store,
        mail: &mut rig.mail,
        registry: &registry,
        decide: &decide,
        def: load(FlowKind::Build)?,
        run,
    };
    body(&mut cx)
}

#[test]
fn the_attempt_comes_from_the_step_jobs_row_not_the_job_payload() -> R {
    let mut rig = rig()?;
    let slice = built_slice(&mut rig)?;
    for job in rig.jobs.jobs.values_mut() {
        if let Some(payload) = job.payload.as_object_mut() {
            payload.remove("attempt");
        }
    }
    assert!(
        rig.jobs
            .jobs
            .values()
            .all(|job| job.payload.get("attempt").is_none())
    );

    rig.store.insert_step_job(
        NEED,
        &StepJobRow {
            job: "slice-second".into(),
            run: 1,
            node: slice.node.clone(),
            task: slice.task.clone(),
            attempt: 2,
        },
    )?;

    let attempt = with_cx(&mut rig, |cx| cx.current_attempt(&slice.task))?;

    assert_eq!(attempt, 2);
    Ok(())
}

#[test]
fn a_task_with_no_step_jobs_row_is_an_error_naming_the_task() -> R {
    let mut rig = rig()?;
    start(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("plan"))?;

    let err = with_cx(&mut rig, |cx| cx.current_attempt("t-none")).err();

    let text = err.map(|e| e.to_string()).unwrap_or_default();
    assert!(
        text.contains("slice t-none: expected a step_jobs row; actual: none"),
        "{text}"
    );
    Ok(())
}

/// A finished hand job whose receipt says `body`.
fn hand_job(body: &str) -> PoolJob {
    let mut job = PoolJob::from(crate::pump::jobs::JobSpec::new(
        "hand-1", "hand", "pool", "s", "b",
    ));
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: body.to_owned(),
        at: 1,
    });
    job
}

#[test]
fn a_provider_failure_alone_is_a_host_setup_environment_failure() {
    let job = hand_job("agent exit 1 in 8s\n\n--- stderr tail\nConnection refused\n");

    let env = runner_env_failure(&job).expect("runner fact");

    assert_eq!(env.kind, EnvKind::HostSetup);
    assert!(env.evidence.contains("provider"), "{}", env.evidence);
}

#[test]
fn an_endpoint_failure_is_an_environment_failure() {
    let job = hand_job("agent exit 1 in 8s\n\n--- stderr tail\n503 Service Unavailable\n");

    let env = runner_env_failure(&job).expect("runner fact");

    assert_eq!(env.kind, EnvKind::HostSetup);
}

#[test]
fn a_shell_marker_only_failure_is_the_hands_judgment() {
    let job = hand_job("agent exit 1 in 8s\n\n--- stderr tail\nbash: tsc: command not found\n");

    assert_eq!(runner_env_failure(&job), None);
}

#[test]
fn a_successful_agent_is_never_an_environment_failure() {
    let job = hand_job(
        "agent exit 0 in 8s\n\n--- stderr tail\nConnection refused\nbash: tsc: command not found\n",
    );

    assert_eq!(runner_env_failure(&job), None);
}

#[test]
fn agent_exit_127_is_a_missing_tool() {
    let job = hand_job("agent exit 127 in 1s");

    let env = runner_env_failure(&job).expect("runner fact");

    assert_eq!(env.kind, EnvKind::ToolMissing);
    assert_eq!(env.evidence, "agent exit 127");
}
