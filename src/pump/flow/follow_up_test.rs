//! Tests for the follow-up routing hook.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::FollowUpRework;
use crate::error::VivariumError;
use crate::mailspace::shipped::SLICER_ROLE;
use crate::pump::config::PumpConfig;
use crate::pump::envelope::roles::{FollowUp, Hand, HandAnswer};
use crate::pump::flow::collect_slice::BuiltHook;
use crate::pump::flow::collect_step::RunCx;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::flow_links::SliceLink;
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";

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
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
    run: RunRow,
    slice: StepRow,
}

fn step(node: &str, task: &str) -> StepRow {
    StepRow {
        run: 1,
        node: node.into(),
        task: task.into(),
        state: StepState::Running,
        verdict: None,
    }
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let mut routing = (*ctx.routing).clone();
    routing.roles.retain(|role| role.name != SLICER_ROLE);
    routing.roles.push(RoutedRole {
        name: SLICER_ROLE.into(),
        kind: Some(String::from("slicer")),
        status: String::from("active"),
        pool: Some(String::from("test-slicer-pool")),
        read_only: None,
        model: None,
        models: Vec::new(),
    });
    let ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let run = RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    };
    store.insert_run(&run)?;
    let mut mail = FakeMail::new(&ctx.cfg.reply_from);
    let sentinel = mail.send_step("pump", "pump", "run", "sentinel", &[])?;
    let slice_task = mail.send_step("pump", "pump", "s", "slice body text", &[])?;
    store.insert_step(NEED, &step("sentinel", &sentinel))?;
    let slice = step("slice:a", &slice_task);
    store.insert_step(NEED, &slice)?;
    store.link_slice(&SliceLink {
        task: slice_task,
        need: NEED.into(),
        run: 1,
        fanout: Some("fan1".into()),
        deps: Vec::new(),
    })?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
        run,
        slice,
    })
}

fn item(need: &str, optional: bool) -> FollowUp {
    FollowUp {
        paths: vec!["src/a.rs".into(), "src/b.rs".into()],
        need: need.into(),
        optional,
    }
}

fn hand(follow_up: Vec<FollowUp>) -> Hand {
    Hand {
        answer: HandAnswer::Built,
        change: Vec::new(),
        check: Vec::new(),
        deviations: Vec::new(),
        follow_up,
        remaining: Vec::new(),
        scope_paths: Vec::new(),
        scope_reason: None,
        scope_design: false,
    }
}

/// Run the hook once on the rig's slice.
fn run_hook(rig: &mut Rig, envelope: &Hand) -> R {
    let registry: Registry<Jobs, FakeMail> = Registry::new();
    let decide = |_: &_, _: &_| Ok(String::new());
    let mut cx = RunCx {
        ctx: &rig.ctx,
        st: &mut rig.jobs,
        store: &rig.store,
        mail: &mut rig.mail,
        registry: &registry,
        decide: &decide,
        def: load(FlowKind::Build)?,
        run: rig.run.clone(),
    };
    FollowUpRework.built(&mut cx, &rig.slice, envelope)
}

#[test]
fn only_the_required_follow_up_queues_a_follow_up_reslice_job() -> R {
    let mut rig = rig()?;
    let envelope = hand(vec![item("fix the parser", false), item("tidy docs", true)]);

    run_hook(&mut rig, &envelope)?;

    assert_eq!(rig.jobs.jobs.len(), 1);
    let job = rig.jobs.jobs.values().next().expect("job");
    assert_eq!(job.payload["mode"], "follow-up");
    assert!(job.body.contains("fix the parser"), "{}", job.body);
    assert!(job.body.contains("src/a.rs, src/b.rs"), "{}", job.body);
    assert!(!job.body.contains("tidy docs"), "{}", job.body);
    let task = job.task.clone().expect("task");
    let body = rig.mail.tasks[&task].body.clone();
    assert!(body.contains("follow-up from slice slice:a"), "{body}");
    assert!(body.contains("Answer sliced to keep this work"), "{body}");
    Ok(())
}

#[test]
fn optional_follow_ups_queue_nothing() -> R {
    let mut rig = rig()?;

    run_hook(&mut rig, &hand(vec![item("tidy docs", true)]))?;

    assert!(rig.jobs.jobs.is_empty());
    Ok(())
}

#[test]
fn calling_the_hook_twice_queues_one_job() -> R {
    let mut rig = rig()?;
    let envelope = hand(vec![item("fix the parser", false)]);

    run_hook(&mut rig, &envelope)?;
    run_hook(&mut rig, &envelope)?;

    assert_eq!(rig.jobs.jobs.len(), 1);
    Ok(())
}
