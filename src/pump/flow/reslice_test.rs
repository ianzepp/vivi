//! Tests for reslice requests.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::{KIND, Mode, Reslices, request};
use crate::error::VivariumError;
use crate::mailspace::shipped::SLICER_ROLE;
use crate::pump::config::PumpConfig;
use crate::pump::envelope::roles::Slice;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::fanout;
use crate::pump::flow::handlers::TickPhase;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::MailReader;
use crate::pump::flow::mail::MailWriter;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob};
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

// ---- the reslices phase ----

const SPLIT_REPLY: &str = "+++\nanswer = \"sliced\"\nmode = \"split\"\n\n\
[[slice]]\nid = \"A.1\"\ntitle = \"t1\"\nbrief = \"b1\"\ntarget = \"vivi\"\n\n\
[[slice]]\nid = \"A.2\"\ntitle = \"t2\"\nbrief = \"b2\"\ntarget = \"vivi\"\n+++\n";

const REWORK_REPLY: &str = "+++\nanswer = \"sliced\"\nmode = \"rework\"\n\n\
[[slice]]\nid = \"A2\"\ntitle = \"t\"\nbrief = \"b\"\ntarget = \"vivi\"\n+++\n";

fn planted(id: &str) -> Slice {
    Slice {
        id: id.into(),
        title: format!("title {id}"),
        brief: format!("brief {id}"),
        target: "vivi".into(),
        write_scope: Vec::new(),
        deps: Vec::new(),
    }
}

/// A rig whose slice A was fanned out, plus the task a split request queued.
fn phase_rig() -> Result<(Rig, String), VivariumError> {
    let mut rig = rig()?;
    let slicer_task = rig.mail.send_step("pump", "pump", "slice", "slicer", &[])?;
    let slicer = step("slice", &slicer_task);
    rig.store.insert_step(NEED, &slicer)?;
    fanout::expand(
        &rig.ctx,
        &rig.store,
        &mut rig.mail,
        &rig.run,
        &slicer,
        &[planted("A")],
    )?;
    let task = slice_task_of(&rig, "A")?;
    rig.slice = rig.store.step_by_task(&task)?.expect("slice row");
    let reslice = ask(&mut rig, Mode::Split, "too big")?;
    Ok((rig, reslice))
}

fn slice_task_of(rig: &Rig, id: &str) -> Result<String, VivariumError> {
    let node = fanout::slice_node(id);
    rig.store
        .steps_of(NEED, 1)?
        .into_iter()
        .find(|row| row.node == node)
        .map(|row| row.task)
        .ok_or_else(|| VivariumError::Parse(format!("no slice row {id}")))
}

fn end_job(rig: &mut Rig, task: &str, status: JobStatus, message: &str) {
    let job = rig
        .jobs
        .jobs
        .get_mut(&format!("reslice-{task}-a1"))
        .expect("job");
    job.status = status;
    if status == JobStatus::Finished {
        job.outcome = Some(JobOutcome {
            state: "finished".into(),
            detail: String::new(),
            receipt: format!("pool result j1: finished\n\n--- receipt\n{message}"),
            at: 0,
        });
    }
}

fn run_phase(rig: &mut Rig) -> R {
    TickPhase::run(
        &Reslices,
        &rig.ctx,
        &mut rig.jobs,
        &rig.store,
        &mut rig.mail,
    )
}

fn verdict(rig: &Rig, task: &str) -> Result<Option<String>, VivariumError> {
    Ok(rig.mail.task_state(task)?.verdict)
}

#[test]
fn a_finished_split_job_replaces_the_slice_and_closes_the_reslice_task_sliced() -> R {
    let (mut rig, task) = phase_rig()?;
    end_job(&mut rig, &task, JobStatus::Finished, SPLIT_REPLY);

    run_phase(&mut rig)?;

    assert_eq!(verdict(&rig, &rig.slice.task)?.as_deref(), Some("replaced"));
    slice_task_of(&rig, "A.1")?;
    slice_task_of(&rig, "A.2")?;
    assert_eq!(verdict(&rig, &task)?.as_deref(), Some("sliced"));
    let row = rig.store.step_by_task(&task)?.expect("row");
    assert_eq!(row.state, StepState::Done);
    let job = &rig.jobs.jobs[&format!("reslice-{task}-a1")];
    assert!(job.applied_at.is_some());
    Ok(())
}

#[test]
fn a_timed_out_job_exhausts_the_slice_and_closes_failed() -> R {
    let (mut rig, task) = phase_rig()?;
    end_job(&mut rig, &task, JobStatus::Timeout, "");

    run_phase(&mut rig)?;

    let old = rig.store.step_by_task(&rig.slice.task)?.expect("row");
    assert_eq!(old.state, StepState::Failed);
    assert_eq!(verdict(&rig, &task)?.as_deref(), Some("failed"));
    Ok(())
}

#[test]
fn a_mode_mismatch_counts_as_failed() -> R {
    let (mut rig, task) = phase_rig()?;
    end_job(&mut rig, &task, JobStatus::Finished, REWORK_REPLY);

    run_phase(&mut rig)?;

    let old = rig.store.step_by_task(&rig.slice.task)?.expect("row");
    assert_eq!(old.state, StepState::Failed);
    assert_eq!(verdict(&rig, &task)?.as_deref(), Some("failed"));
    assert!(slice_task_of(&rig, "A2").is_err());
    Ok(())
}

#[test]
fn a_job_of_a_superseded_run_is_marked_applied_with_no_other_effect() -> R {
    let (mut rig, task) = phase_rig()?;
    end_job(&mut rig, &task, JobStatus::Finished, SPLIT_REPLY);
    rig.store.set_run_state(NEED, 1, "superseded")?;

    run_phase(&mut rig)?;

    let job = &rig.jobs.jobs[&format!("reslice-{task}-a1")];
    assert!(job.applied_at.is_some());
    assert_eq!(verdict(&rig, &task)?, None);
    let row = rig.store.step_by_task(&task)?.expect("row");
    assert_eq!(row.state, StepState::Running);
    assert!(slice_task_of(&rig, "A.1").is_err());
    Ok(())
}
