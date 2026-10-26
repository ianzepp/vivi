//! Tests for reslice requests.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::{KIND, Mode, RESLICE_FAILED, Reslices, request, settle_error};
use crate::error::VivariumError;
use crate::mailspace::shipped::SLICER_ROLE;
use crate::pump::config::PumpConfig;
use crate::pump::envelope::roles::Slice;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::fanout;
use crate::pump::flow::handlers::TickPhase;
use crate::pump::flow::index::{JOB_STALE, RunRow, StepRow, StepState};
use crate::pump::flow::mail::MailReader;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::step_error::STEP_ERROR_HELD;
use crate::pump::flow::tick::OPERATOR_HOLD;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::rung_result::ENV_FAILURE;
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
    rig_with(PumpConfig::default())
}

fn rig_with(cfg: PumpConfig) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), cfg);
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
    let step = crate::letter::read::step_of(&body).expect("step");
    assert_eq!((step.need.as_str(), step.run), (NEED, 1));
    assert_eq!(step.node.as_deref(), Some("reslice:a-1"));
    assert!(body.contains("\n\nmode: split\nslice: "), "{body}");
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
    assert_eq!(rig.store.last_attempt(&task)?, 0);
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

#[test]
fn reslice_jobs_budget_as_slicers_for_every_initial_mode() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 222;
    cfg.dispatch.step_budgets.slicer = Some(300);
    // Another thinking kind configured: it must not reach a reslice job.
    cfg.dispatch.step_budgets.judge = Some(88);
    let mut rig = rig_with(cfg)?;

    let split = ask(&mut rig, Mode::Split, "too big")?;
    let rework = ask(&mut rig, Mode::Rework, "redo it")?;
    let follow = ask(&mut rig, Mode::FollowUp, "then this")?;

    for (task, mode) in [
        (&split, "split"),
        (&rework, "rework"),
        (&follow, "follow-up"),
    ] {
        let job = &rig.jobs.jobs[&format!("reslice-{task}-a1")];
        // The configured slicer override wins under the durable `reslice`
        // kind; the judge override never reaches it.
        assert_eq!(job.budget, 300, "{mode}");
        assert_eq!(job.payload["mode"], mode, "{mode}");
        assert_eq!(job.payload["slice"], rig.slice.task, "{mode}");
        assert_eq!(job.task.as_deref(), Some(task.as_str()), "{mode}");
        assert!(job.read_only, "{mode}");
        assert_eq!(
            job.from_ref.as_deref(),
            Some(rig.ctx.cfg.main.as_str()),
            "{mode}"
        );
        assert_eq!(
            job.extra_targets,
            crate::pump::flow::step_inputs::targets(&rig.ctx),
            "{mode}"
        );
        // Slice sizing guidance keeps the default minutes; the execution
        // budget is never a sizing input.
        assert_eq!(rig.ctx.cfg.slice.minutes(), 10);
        assert!(
            job.body.contains("slice size: aim for 5 to 10 minutes"),
            "{}",
            job.body
        );
    }
    assert_eq!(rig.store.last_attempt(&split)?, 0);
    Ok(())
}

#[test]
fn reslice_jobs_without_a_slicer_override_take_the_changed_fallback() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 222;
    cfg.dispatch.step_budgets.planner = Some(90);
    let mut rig = rig_with(cfg)?;

    let task = ask(&mut rig, Mode::Rework, "redo it")?;
    let before = sends(&rig);

    // No slicer key: the job takes the changed shared budget, exactly.
    let job = &rig.jobs.jobs[&format!("reslice-{task}-a1")];
    assert_eq!(job.budget, 222);
    assert_eq!(job.budget, rig.ctx.cfg.dispatch.budget);
    // Idempotence survives the changed budgets: the same request again sends
    // nothing and books no second attempt.
    let again = ask(&mut rig, Mode::Rework, "redo it")?;
    assert_eq!(again, task);
    assert_eq!(sends(&rig), before);
    assert_eq!(rig.jobs.jobs.len(), 1);
    assert_eq!(rig.store.last_attempt(&task)?, 0);
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
    phase_rig_with(PumpConfig::default(), Mode::Split, "too big")
}

/// A rig whose slice A was fanned out, plus the task a `mode` request queued.
fn phase_rig_with(
    cfg: PumpConfig,
    mode: Mode,
    input: &str,
) -> Result<(Rig, String), VivariumError> {
    let mut rig = rig_with(cfg)?;
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
    let reslice = ask(&mut rig, mode, input)?;
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

fn end_id(rig: &mut Rig, id: &str, status: JobStatus, message: &str) {
    let job = rig.jobs.jobs.get_mut(id).expect("job");
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

fn end_job(rig: &mut Rig, task: &str, status: JobStatus, message: &str) {
    end_id(rig, &format!("reslice-{task}-a1"), status, message);
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

    // The first kill raises its one retry; the retry's own kill fails.
    run_phase(&mut rig)?;
    let copy = format!("reslice-{task}-a2");
    assert_eq!(rig.jobs.jobs[&copy].status, JobStatus::Pending);
    end_id(&mut rig, &copy, JobStatus::Timeout, "");

    run_phase(&mut rig)?;

    let old = rig.store.step_by_task(&rig.slice.task)?.expect("row");
    assert_eq!(old.state, StepState::Failed);
    assert_eq!(verdict(&rig, &task)?.as_deref(), Some("failed"));
    Ok(())
}

// ---- budget kills: one raised retry, then the failure ----

/// The two ways the pool reports a budget kill: the terminal Timeout status,
/// and a finished job whose bounded summary reports agent exit 124.
#[derive(Debug, Clone, Copy)]
enum Kill {
    Status,
    Exit124,
}

impl Kill {
    /// End the job `id` as this kind of budget kill.
    fn plant(self, rig: &mut Rig, id: &str) {
        match self {
            Self::Status => end_id(rig, id, JobStatus::Timeout, ""),
            Self::Exit124 => end_receipt(rig, id, agent_receipt(124, "", "ran out of time")),
        }
    }
}

fn reply_for(mode: Mode) -> &'static str {
    if mode == Mode::Split {
        SPLIT_REPLY
    } else {
        REWORK_REPLY
    }
}

#[test]
fn reslice_budget_kill_retries_same_request_once() -> R {
    for mode in [Mode::Split, Mode::Rework, Mode::FollowUp] {
        for kill in [Kill::Status, Kill::Exit124] {
            raised_kill_retries_same_request(mode, kill)?;
            second_kill_fails(mode, kill)?;
        }
    }
    Ok(())
}

/// The first kill raises one copy of the same request at the doubled budget;
/// the raised copy then applies the ordinary way, exactly once.
fn raised_kill_retries_same_request(mode: Mode, kill: Kill) -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 222;
    let (mut rig, task) = phase_rig_with(cfg, mode, "too big")?;
    let first = format!("reslice-{task}-a1");
    let id = format!("reslice-{task}-a2");
    let original = rig.jobs.jobs[&first].clone();
    let before = sends(&rig);
    kill.plant(&mut rig, &first);

    run_phase(&mut rig)?;
    run_phase(&mut rig)?; // a repeated phase queues no second copy
    assert_retry_raised(&rig, &first, &id, &task, mode, original, before)?;

    end_id(&mut rig, &id, JobStatus::Finished, reply_for(mode));
    run_phase(&mut rig)?;

    assert!(rig.jobs.jobs[&id].applied_at.is_some());
    let row = rig.store.step_by_task(&task)?.expect("reslice row");
    assert_eq!(row.state, StepState::Done);
    assert_eq!(verdict(&rig, &task)?.as_deref(), Some("sliced"));
    match mode {
        Mode::Split => {
            slice_task_of(&rig, "A.1")?;
            slice_task_of(&rig, "A.2")?;
        }
        Mode::Rework | Mode::FollowUp => {
            slice_task_of(&rig, "A2")?;
        }
    }
    Ok(())
}

/// The kill left the request context untouched: applied original, one raised
/// copy of the same request on the still-Running reslice task, no new task,
/// no premature downstream slice or closure.
fn assert_retry_raised(
    rig: &Rig,
    first: &str,
    id: &str,
    task: &str,
    mode: Mode,
    original: PoolJob,
    sends_before: usize,
) -> R {
    assert!(rig.jobs.jobs[first].applied_at.is_some());
    assert_eq!(rig.jobs.jobs.len(), 2);
    assert_eq!(count_of(rig, RESLICE_FAILED), 0, "{}", exceptions(rig));
    let row = rig.store.step_by_task(task)?.expect("reslice row");
    assert_eq!(row.state, StepState::Running);
    assert_eq!(verdict(rig, task)?, None);
    assert_eq!(sends(rig), sends_before);
    assert!(slice_task_of(rig, "A.1").is_err());
    assert!(slice_task_of(rig, "A2").is_err());
    assert_ne!(
        rig.store
            .step_by_task(&rig.slice.task)?
            .expect("slice row")
            .state,
        StepState::Failed
    );
    // 222 doubled is 444; every other input is the original's
    let copy = &rig.jobs.jobs[id];
    assert_eq!(copy.status, JobStatus::Pending);
    assert_eq!(copy.budget, 444);
    let mut expected = original;
    expected.id = id.to_owned();
    expected.queued_at = copy.queued_at;
    expected.budget = 444;
    expected.payload["budget_retry"] = serde_json::Value::Bool(true);
    assert_eq!(copy, &expected);
    assert_eq!(copy.payload["slice"], rig.slice.task);
    assert_eq!(copy.payload["task"], task);
    assert_eq!(copy.payload["mode"], mode.name());
    Ok(())
}

/// A second kill has no allowance: the existing failed-reslice application
/// runs even though a valid envelope would parse, and no third job appears.
fn second_kill_fails(mode: Mode, kill: Kill) -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 222;
    let (mut rig, task) = phase_rig_with(cfg, mode, "too big")?;
    let first = format!("reslice-{task}-a1");
    let id = format!("reslice-{task}-a2");
    kill.plant(&mut rig, &first);
    run_phase(&mut rig)?;
    kill.plant(&mut rig, &id);

    run_phase(&mut rig)?;
    run_phase(&mut rig)?;

    assert!(rig.jobs.jobs[&first].applied_at.is_some());
    assert!(rig.jobs.jobs[&id].applied_at.is_some());
    assert_eq!(rig.jobs.jobs.len(), 2);
    assert_eq!(count_of(&rig, RESLICE_FAILED), 1, "{}", exceptions(&rig));
    assert!(
        exceptions(&rig).contains("budget kill"),
        "{}",
        exceptions(&rig)
    );
    let row = rig.store.step_by_task(&task)?.expect("reslice row");
    assert_eq!(row.state, StepState::Done);
    assert_eq!(verdict(&rig, &task)?.as_deref(), Some("failed"));
    assert!(slice_task_of(&rig, "A.1").is_err());
    assert!(slice_task_of(&rig, "A2").is_err());
    let old = rig.store.step_by_task(&rig.slice.task)?.expect("slice row");
    if mode == Mode::FollowUp {
        // a failed follow-up escalates instead of exhausting its slice
        assert_ne!(old.state, StepState::Failed);
    } else {
        assert_eq!(old.state, StepState::Failed);
    }
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
    assert_eq!(count_of(&rig, JOB_STALE), 0, "{}", exceptions(&rig));
    Ok(())
}

#[test]
fn a_job_of_a_withdrawn_run_is_marked_applied_with_no_alarm() -> R {
    let (mut rig, task) = phase_rig()?;
    end_job(&mut rig, &task, JobStatus::Finished, SPLIT_REPLY);
    rig.store.set_run_state(NEED, 1, "withdrawn")?;

    run_phase(&mut rig)?;

    let job = &rig.jobs.jobs[&format!("reslice-{task}-a1")];
    assert!(job.applied_at.is_some());
    assert_eq!(count_of(&rig, JOB_STALE), 0, "{}", exceptions(&rig));
    assert!(slice_task_of(&rig, "A.1").is_err());
    Ok(())
}

#[test]
fn a_job_of_a_done_run_raises_one_stale_job_and_is_applied() -> R {
    let (mut rig, task) = phase_rig()?;
    end_job(&mut rig, &task, JobStatus::Finished, SPLIT_REPLY);
    rig.store.set_run_state(NEED, 1, "done")?;

    run_phase(&mut rig)?;
    run_phase(&mut rig)?;

    assert_eq!(count_of(&rig, JOB_STALE), 1, "{}", exceptions(&rig));
    assert!(exceptions(&rig).contains("is done"), "{}", exceptions(&rig));
    assert!(
        rig.jobs.jobs[&format!("reslice-{task}-a1")]
            .applied_at
            .is_some()
    );
    Ok(())
}

#[test]
fn a_job_naming_a_run_that_does_not_exist_raises_one_stale_job() -> R {
    let (mut rig, task) = phase_rig()?;
    end_job(&mut rig, &task, JobStatus::Finished, SPLIT_REPLY);
    set_payload_run(&mut rig, &task, Some(7));

    run_phase(&mut rig)?;
    run_phase(&mut rig)?;

    assert_eq!(count_of(&rig, JOB_STALE), 1, "{}", exceptions(&rig));
    assert!(
        exceptions(&rig).contains("live runs of"),
        "{}",
        exceptions(&rig)
    );
    Ok(())
}

#[test]
fn a_duplicate_id_split_reply_exhausts_the_slice_and_marks_the_job_applied() -> R {
    let (mut rig, task) = phase_rig()?;
    let duplicated = SPLIT_REPLY.replace("A.2", "A.1");
    end_job(&mut rig, &task, JobStatus::Finished, &duplicated);

    run_phase(&mut rig)?;

    let old = rig.store.step_by_task(&rig.slice.task)?.expect("row");
    assert_eq!(old.state, StepState::Failed);
    assert_eq!(verdict(&rig, &task)?.as_deref(), Some("failed"));
    let job = &rig.jobs.jobs[&format!("reslice-{task}-a1")];
    assert!(job.applied_at.is_some());
    assert!(slice_task_of(&rig, "A.1").is_err());
    Ok(())
}

// ---- env failures and persistent errors ----

fn agent_receipt(exit: i32, message: &str, stderr: &str) -> String {
    format!(
        "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
         push: none\nagent exit {exit} in 12s, job 20s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n{stderr}\n\n--- runner log tail\nlog line"
    )
}

fn end_receipt(rig: &mut Rig, id: &str, receipt: String) {
    let job = rig.jobs.jobs.get_mut(id).expect("job");
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt,
        at: 0,
    });
}

fn end_job_receipt(rig: &mut Rig, task: &str, receipt: String) {
    end_receipt(rig, &format!("reslice-{task}-a1"), receipt);
}

fn exceptions(rig: &Rig) -> String {
    std::fs::read_to_string(rig.ctx.home.join("exceptions.txt")).unwrap_or_default()
}

fn count_of(rig: &Rig, kind: &str) -> usize {
    exceptions(rig)
        .lines()
        .filter(|line| line.contains(kind))
        .count()
}

#[test]
fn an_agent_exit_127_holds_the_reslice_row_and_exhausts_nothing() -> R {
    let (mut rig, task) = phase_rig()?;
    let text = agent_receipt(127, "", "bash: claude: command not found");
    end_job_receipt(&mut rig, &task, text);

    run_phase(&mut rig)?;

    let row = rig.store.step_by_task(&task)?.expect("row");
    assert_eq!(row.state, StepState::Waiting);
    assert_eq!(row.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(count_of(&rig, ENV_FAILURE), 1, "{}", exceptions(&rig));
    assert_eq!(count_of(&rig, RESLICE_FAILED), 0);
    let old = rig.store.step_by_task(&rig.slice.task)?.expect("slice");
    assert_ne!(old.state, StepState::Failed);
    assert_eq!(verdict(&rig, &task)?, None);
    let job = &rig.jobs.jobs[&format!("reslice-{task}-a1")];
    assert!(job.applied_at.is_some());
    Ok(())
}

#[test]
fn a_missing_tool_in_the_stderr_tail_of_an_exit_one_is_not_held() -> R {
    let (mut rig, task) = phase_rig()?;
    let text = agent_receipt(1, "no envelope", "bash: cargo: command not found");
    end_job_receipt(&mut rig, &task, text);

    run_phase(&mut rig)?;

    let row = rig.store.step_by_task(&task)?.expect("row");
    assert_ne!(row.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(count_of(&rig, ENV_FAILURE), 0);
    assert_eq!(count_of(&rig, RESLICE_FAILED), 1);
    let old = rig.store.step_by_task(&rig.slice.task)?.expect("slice");
    assert_eq!(old.state, StepState::Failed);
    Ok(())
}

#[test]
fn a_store_that_keeps_failing_holds_the_row_after_three_ticks() -> R {
    let (mut rig, task) = phase_rig()?;
    end_job(&mut rig, &task, JobStatus::Finished, SPLIT_REPLY);
    rusqlite::Connection::open(rig._dir.path().join(DB_FILENAME))
        .and_then(|conn| conn.execute_batch("DROP TABLE flow_runs"))
        .map_err(|e| VivariumError::Other(e.to_string()))?;
    let id = format!("reslice-{task}-a1");

    for _ in 0..2 {
        assert!(run_phase(&mut rig).is_err());
        assert!(rig.jobs.jobs[&id].applied_at.is_none());
    }
    run_phase(&mut rig)?;

    assert!(rig.jobs.jobs[&id].applied_at.is_some());
    let row = rig.store.step_by_task(&task)?.expect("row");
    assert_eq!(row.state, StepState::Waiting);
    assert_eq!(row.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(count_of(&rig, STEP_ERROR_HELD), 1, "{}", exceptions(&rig));
    Ok(())
}

// ---- payload identity ----

fn set_payload_run(rig: &mut Rig, task: &str, run: Option<u32>) {
    let job = rig
        .jobs
        .jobs
        .get_mut(&format!("reslice-{task}-a1"))
        .expect("job");
    let fields = job.payload.as_object_mut().expect("object payload");
    match run {
        Some(n) => fields.insert("run".into(), serde_json::json!(n)),
        None => fields.remove("run"),
    };
}

#[test]
fn a_payload_without_a_run_raises_one_stale_job_naming_run_and_is_applied() -> R {
    let (mut rig, task) = phase_rig()?;
    end_job(&mut rig, &task, JobStatus::Finished, SPLIT_REPLY);
    set_payload_run(&mut rig, &task, None);

    run_phase(&mut rig)?;
    run_phase(&mut rig)?;

    assert_eq!(count_of(&rig, JOB_STALE), 1, "{}", exceptions(&rig));
    assert!(exceptions(&rig).contains("`run`"), "{}", exceptions(&rig));
    assert!(
        rig.jobs.jobs[&format!("reslice-{task}-a1")]
            .applied_at
            .is_some()
    );
    assert_eq!(verdict(&rig, &task)?, None);
    Ok(())
}

#[test]
fn a_payload_run_that_differs_from_the_step_row_raises_one_stale_job_naming_both() -> R {
    let (mut rig, task) = phase_rig()?;
    end_job(&mut rig, &task, JobStatus::Finished, SPLIT_REPLY);
    let second = RunRow {
        run: 2,
        ..rig.run.clone()
    };
    rig.store.insert_run(&second)?;
    set_payload_run(&mut rig, &task, Some(2));

    run_phase(&mut rig)?;

    assert_eq!(count_of(&rig, JOB_STALE), 1, "{}", exceptions(&rig));
    let text = exceptions(&rig);
    assert!(text.contains("r1") && text.contains("r2"), "{text}");
    assert!(
        rig.jobs.jobs[&format!("reslice-{task}-a1")]
            .applied_at
            .is_some()
    );
    assert_eq!(verdict(&rig, &task)?, None);
    assert!(slice_task_of(&rig, "A.1").is_err());
    Ok(())
}

#[test]
fn a_closed_step_is_a_late_result_applied_with_no_exception() -> R {
    let (mut rig, task) = phase_rig()?;
    end_job(&mut rig, &task, JobStatus::Finished, SPLIT_REPLY);
    rig.store
        .set_step_state(&task, StepState::Done, Some("sliced"))?;

    run_phase(&mut rig)?;

    assert_eq!(count_of(&rig, JOB_STALE), 0, "{}", exceptions(&rig));
    assert!(
        rig.jobs.jobs[&format!("reslice-{task}-a1")]
            .applied_at
            .is_some()
    );
    assert!(slice_task_of(&rig, "A.1").is_err());
    Ok(())
}

#[test]
fn a_job_with_no_payload_task_raises_its_own_held_alarm_when_it_gives_up() -> R {
    let (mut rig, task) = phase_rig()?;
    end_job(&mut rig, &task, JobStatus::Finished, SPLIT_REPLY);
    let id = format!("reslice-{task}-a1");
    let job = rig.jobs.jobs.get_mut(&id).expect("job");
    job.payload
        .as_object_mut()
        .expect("object payload")
        .remove("task");
    let job = job.clone();

    for _ in 0..2 {
        let kept = settle_error(&rig.ctx, &mut rig.jobs, &rig.store, &job, failure());
        assert!(kept.is_some());
        assert_eq!(count_of(&rig, STEP_ERROR_HELD), 0);
    }
    let kept = settle_error(&rig.ctx, &mut rig.jobs, &rig.store, &job, failure());

    assert!(kept.is_none());
    assert!(rig.jobs.jobs[&id].applied_at.is_some());
    assert_eq!(count_of(&rig, STEP_ERROR_HELD), 1, "{}", exceptions(&rig));
    let text = exceptions(&rig);
    assert!(text.contains(&id), "{text}");
    assert!(text.contains("expected a payload task to hold"), "{text}");
    Ok(())
}

fn failure() -> VivariumError {
    VivariumError::Other("store down".into())
}
