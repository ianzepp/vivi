//! Tests for reslice requests.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::{KIND, Mode, request};
use crate::error::VivariumError;
use crate::mailspace::shipped::SLICER_ROLE;
use crate::pump::config::PumpConfig;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
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
        flow_version: 1,
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

fn ask(rig: &mut Rig, mode: Mode, input: &str) -> Result<String, VivariumError> {
    request(
        &rig.ctx,
        &mut rig.jobs,
        &rig.store,
        &mut rig.mail,
        &rig.run,
        &rig.slice,
        mode,
        input,
    )
}

fn sends(rig: &Rig) -> usize {
    rig.mail
        .calls
        .iter()
        .filter(|call| matches!(call, Call::SendStep { .. }))
        .count()
}

#[test]
fn one_request_sends_a_bound_task_a_row_a_link_and_a_job() -> R {
    let mut rig = rig()?;
    let before = sends(&rig);

    let task = ask(&mut rig, Mode::Split, "too big")?;

    assert_eq!(sends(&rig), before + 1);
    let body = rig.mail.tasks[&task].body.clone();
    assert!(body.starts_with("pump-step: n1 r1 reslice:a-1\nmode: split\nslice: "));
    assert!(body.ends_with("\n\ntoo big"), "{body}");
    let row = rig.store.step_by_task(&task)?.expect("row");
    assert_eq!(row.node, "reslice:a-1");
    assert_eq!(row.state, StepState::Running);
    let link = rig.store.slice_link(&task)?.expect("link");
    assert_eq!(link.fanout.as_deref(), Some("fan1"));
    let job = &rig.jobs.jobs[&format!("reslice-{task}-a1")];
    assert_eq!(job.kind, KIND);
    assert_eq!(job.task.as_deref(), Some(task.as_str()));
    assert_eq!(job.payload["slice"], rig.slice.task);
    assert_eq!(job.payload["mode"], "split");
    assert!(
        job.body.contains("slice size: aim for 5 to"),
        "{}",
        job.body
    );
    assert!(job.body.contains("slice body text"), "{}", job.body);
    assert_eq!(rig.store.attempts_of(&task)?, 0);
    Ok(())
}

#[test]
fn the_same_request_again_sends_nothing() -> R {
    let mut rig = rig()?;
    let first = ask(&mut rig, Mode::Rework, "redo it")?;
    let before = sends(&rig);

    let again = ask(&mut rig, Mode::Rework, "redo it")?;

    assert_eq!(again, first);
    assert_eq!(sends(&rig), before);
    assert_eq!(rig.jobs.jobs.len(), 1);
    Ok(())
}

#[test]
fn other_input_gets_the_next_node_number() -> R {
    let mut rig = rig()?;
    let first = ask(&mut rig, Mode::Split, "one")?;

    let second = ask(&mut rig, Mode::Split, "two")?;

    assert_ne!(first, second);
    let row = rig.store.step_by_task(&second)?.expect("row");
    assert_eq!(row.node, "reslice:a-2");
    assert_eq!(rig.jobs.jobs.len(), 2);
    Ok(())
}
