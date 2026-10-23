//! Tests for a runner-level environment failure on a slice hand job: the
//! slice is held for the operator and no attempt is used.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::error::VivariumError;
use crate::letter::fixture::task_body_with;
use crate::letter::work::TaskFront;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::need_branch;
use crate::pump::flow::reslice;
use crate::pump::flow::slice_dispatch;
use crate::pump::flow::tick::{OPERATOR_HOLD, tick_with};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::rung_result::ENV_FAILURE;
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const BASE: &str = "2222222222222222222222222222222222222222";
const FAILING_CHECK: &str = "+++\nanswer = \"built\"\n\n[[check]]\ncmd = \"cargo test\"\n\
                             result = \"fail\"\nseconds = 3\n+++\nthe check fails";

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
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
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
        host,
        store,
        mail,
        jobs: Jobs::default(),
    })
}

/// A started run with one dispatched slice whose hand job ended with the
/// given runner `detail`, agent exit code, stderr tail and receipt message.
fn ended(
    rig: &mut Rig,
    detail: &str,
    exit: i32,
    stderr: &str,
    message: &str,
) -> Result<StepRow, VivariumError> {
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
        state: StepState::Running,
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
         push: none\nagent exit {exit} in 12s, job 20s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n{stderr}\n\n--- runner log tail\nlog line"
    );
    let job = rig
        .jobs
        .jobs
        .get_mut(&id)
        .ok_or_else(|| VivariumError::Message("no job".into()))?;
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: detail.into(),
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

fn raised(rig: &Rig) -> usize {
    rig.host
        .logs()
        .iter()
        .filter(|line| line.contains(&format!("EXCEPTION {ENV_FAILURE}")))
        .count()
}

fn environment_replies(rig: &Rig, slice: &StepRow) -> usize {
    rig.mail
        .calls
        .iter()
        .filter(|call| {
            matches!(call, Call::Reply { handle, body }
                if *handle == slice.task && body.contains("environment:"))
        })
        .count()
}

/// The slice is held, one alarm was raised over two ticks, and neither a
/// second attempt nor a reslice job exists.
fn assert_held(rig: &mut Rig, slice: &StepRow) -> R {
    tick(rig)?;
    tick(rig)?;
    let held = rig.store.step_by_task(&slice.task)?.expect("slice row");
    assert_eq!(held.state, StepState::Waiting);
    assert_eq!(held.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(raised(rig), 1, "{:?}", rig.host.logs());
    assert_eq!(environment_replies(rig, slice), 1, "{:?}", rig.mail.calls);
    assert!(jobs_of(rig, reslice::KIND).is_empty());
    let hands = jobs_of(rig, slice_dispatch::KIND);
    assert!(hands.iter().all(|job| attempt(job) == Some(1)), "{hands:?}");
    Ok(())
}

#[test]
fn an_agent_exit_127_holds_the_slice_and_uses_no_attempt() -> R {
    let mut rig = rig()?;
    let slice = ended(&mut rig, "", 127, "", "")?;
    assert_held(&mut rig, &slice)
}

#[test]
fn a_host_failure_receipt_holds_the_slice_and_uses_no_attempt() -> R {
    let mut rig = rig()?;
    let slice = ended(&mut rig, "checkout_failed exited 128", 1, "", "")?;
    assert_held(&mut rig, &slice)
}

#[test]
fn a_built_envelope_with_a_failing_check_is_still_a_failed_attempt() -> R {
    let mut rig = rig()?;
    ended(&mut rig, "", 0, "", FAILING_CHECK)?;

    tick(&mut rig)?;

    assert_eq!(raised(&rig), 0);
    let hands = jobs_of(&rig, slice_dispatch::KIND);
    assert!(hands.iter().any(|job| attempt(job) == Some(2)), "{hands:?}");
    Ok(())
}

#[test]
fn a_command_not_found_only_in_the_stderr_tail_is_a_normal_failed_attempt() -> R {
    let mut rig = rig()?;
    ended(&mut rig, "", 1, "bash: tsc: command not found", "")?;

    tick(&mut rig)?;

    assert_eq!(raised(&rig), 0);
    let hands = jobs_of(&rig, slice_dispatch::KIND);
    assert!(hands.iter().any(|job| attempt(job) == Some(2)), "{hands:?}");
    Ok(())
}
