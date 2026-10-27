//! RC3 test 1 through the tick: the join-close case and the withdrawal case,
//! on a real mailspace.
//!
//! A foreign close of the sentinel and the last open step lets the join close
//! the need. The next tick must reopen both tasks and the need (recording
//! `flow-foreign-close`) and leave the run live, not withdraw it. A need
//! closed while a run task is still open stays a withdrawal.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::{STALE_ATTEMPT, STALE_EXECUTION, tick_with};
use crate::error::VivariumError;
use crate::letter::fixture::{record_body, sentinel_body, step_body};
use crate::mailspace::RoleUpdate;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::guard::FOREIGN_CLOSE;
use crate::pump::flow::advance::start::SENTINEL_NODE;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, MailspacePort, NeedStatus, TaskStatus};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler, role_job};
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;
type Reg = Registry<Jobs, MailspacePort>;

/// The pump's mailspace identity (`reply_from` of the test context).
const PUMP: &str = "operator";

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
    p: Project,
    ctx: Ctx,
    store: PumpStore,
    port: MailspacePort,
    jobs: Jobs,
    need: String,
    sentinel: String,
    step: String,
}

/// A live run with the sentinel and one step sent to `mind`, bound to the
/// need, an open pump record, and rows in the index.
fn rig() -> Result<Rig, VivariumError> {
    let mut p = project();
    p.mailspace.set_role(
        "mind",
        RoleUpdate {
            kind: Some(Some("mind".into())),
            ..RoleUpdate::default()
        },
    )?;
    let mut ctx = p.pump_ctx();
    let mut cfg = (*ctx.cfg).clone();
    cfg.reply_from = PUMP.into();
    ctx.cfg = Arc::new(cfg);
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut port = MailspacePort {
        mailspace: p.mailspace.clone(),
        pump_identity: PUMP.into(),
    };
    let need = p.need("guard through the tick");
    MailWriter::open_record(&mut port, &need, &record_body(&need, "build"))?;
    let sentinel = port.send_step(
        PUMP,
        "mind",
        "r1 sentinel",
        &sentinel_body(&need, 1, ""),
        &[],
    )?;
    let step = port.send_step(
        PUMP,
        "mind",
        "r1 plan",
        &step_body(&need, 1, "plan", ""),
        &[],
    )?;
    port.bind(&need, &[sentinel.clone(), step.clone()])?;
    store.insert_run(&RunRow {
        need: need.clone(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    })?;
    for (node, task, state) in [
        (SENTINEL_NODE, &sentinel, StepState::Running),
        ("plan", &step, StepState::Pending),
    ] {
        store.insert_step(
            &need,
            &StepRow {
                run: 1,
                node: node.into(),
                task: task.clone(),
                state,
                verdict: None,
            },
        )?;
    }
    Ok(Rig {
        p,
        ctx,
        store,
        port,
        jobs: Jobs::default(),
        need,
        sentinel,
        step,
    })
}

impl Rig {
    fn tick(&mut self) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.port,
            &Registry::new(),
        )
    }

    fn tick_run(&mut self, registry: &Reg) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.port,
            registry,
        )
    }

    /// A reply on the need's thread from `from`.
    fn reply_as(&self, from: &str, body: &str) -> R {
        let to = if from == PUMP { "mind" } else { PUMP };
        self.port.mailspace.reply(
            &self.need,
            from,
            vec![to.into()],
            Vec::new(),
            None,
            body.into(),
        )?;
        Ok(())
    }

    /// The message ID of the thread letter whose body holds `body`.
    fn reply_id(&self, body: &str) -> Result<String, VivariumError> {
        let thread = self
            .port
            .mailspace
            .thread(&self.need, false, usize::MAX, usize::MAX)?;
        thread
            .iter()
            .find(|m| m.body.contains(body))
            .map(|m| m.message_id.clone())
            .ok_or_else(|| VivariumError::Message(format!("no thread letter {body:?}")))
    }

    /// The input checkpoint of the execution the plan job activated on.
    fn checkpoint(&self) -> Result<String, VivariumError> {
        self.store
            .execution_by_job(&self.job_id())?
            .map(|row| row.input_checkpoint.unwrap_or_default())
            .ok_or_else(|| VivariumError::Message(String::from("no execution for the plan job")))
    }

    /// The pool job the planner handler enqueues for the plan step.
    fn job_id(&self) -> String {
        format!("step-{}-a1", self.step)
    }

    fn finish(&mut self, id: &str) -> R {
        let job = self.jobs.jobs.get_mut(id).expect("job");
        job.status = JobStatus::Finished;
        Ok(())
    }

    fn mind_closes(&self, task: &str) -> R {
        self.p
            .mailspace
            .move_item_outcome("mind", task, "done", None, "task done", None)?;
        Ok(())
    }

    fn status(&self, task: &str) -> Result<TaskStatus, VivariumError> {
        Ok(self.port.task_state(task)?.status)
    }

    fn exceptions(&self) -> String {
        std::fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }
}

#[test]
fn a_join_close_after_foreign_closes_is_reopened_by_the_tick() -> R {
    let mut rig = rig()?;
    rig.mind_closes(&rig.step.clone())?;
    rig.mind_closes(&rig.sentinel.clone())?;
    rig.p.mailspace.backlog_audit(true)?;
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);

    rig.tick()?;

    assert_eq!(rig.status(&rig.step)?, TaskStatus::Open);
    assert_eq!(rig.status(&rig.sentinel)?, TaskStatus::Open);
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Open);
    let text = rig.exceptions();
    assert_eq!(text.matches(FOREIGN_CLOSE).count(), 2, "{text}");
    assert!(MailReader::open_record(&rig.port, &rig.need)?.is_some());
    assert_eq!(rig.store.live_runs()?.len(), 1, "the run was withdrawn");
    assert!(rig.store.runs_in_state("withdrawn")?.is_empty());
    Ok(())
}

#[test]
fn a_need_closed_with_a_run_task_open_is_still_withdrawn() -> R {
    let mut rig = rig()?;
    rig.mind_closes(&rig.step.clone())?;
    rig.p
        .mailspace
        .move_item_outcome("mind", &rig.need, "done", None, "withdrawn", None)?;
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    assert_eq!(rig.status(&rig.sentinel)?, TaskStatus::Open);

    rig.tick()?;

    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    assert!(rig.store.live_runs()?.is_empty());
    assert_eq!(rig.store.runs_in_state("withdrawn")?.len(), 1);
    assert!(!rig.exceptions().contains(FOREIGN_CLOSE));
    Ok(())
}

/// Starts the plan step by enqueueing its planner job.
struct PlanJob;

impl StepHandler for PlanJob {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::Job(Box::new(role_job(
            cx.ctx,
            cx.run,
            step,
            "planner",
            RoleKind::Planner,
            "planner",
            "the plan brief",
        ))))
    }
}

/// A registry whose planner handler enqueues a job for the plan step.
fn job_registry() -> Reg {
    let mut registry = Registry::new();
    registry.handler(FlowKind::Build, NodeClass::Planner, Box::new(PlanJob));
    registry
}

#[test]
fn a_handler_job_captures_the_need_replies_as_its_input_checkpoint() -> R {
    let mut rig = rig()?;
    rig.reply_as("mind", "the captured input reply")?;
    let registry = job_registry();

    rig.tick_run(&registry)?;

    let job = rig.job_id();
    let execution = rig
        .store
        .execution_by_job(&job)?
        .expect("the enqueued job has an execution");
    assert_eq!(execution.active_job.as_deref(), Some(job.as_str()));
    assert_eq!(execution.phase, "planner");
    assert_eq!(execution.attempt, Some(1));
    assert!(rig.store.execution_is_current(&execution.execution)?);

    // The checkpoint holds exactly the eligible replies at capture: the one
    // member reply, never the root letter, the pump record or an event.
    let captured = rig.reply_id("the captured input reply")?;
    let snapshot = rig.checkpoint()?;
    assert_eq!(snapshot, captured, "{snapshot}");

    // A reply after the enqueue is not part of the captured input, and the
    // snapshot survives the step later parking.
    rig.reply_as("mind", "a reply after the enqueue")?;
    let late = rig.reply_id("a reply after the enqueue")?;
    rig.store
        .set_step_state(&rig.step, StepState::Waiting, None)?;
    let after = rig.checkpoint()?;
    assert_eq!(after, snapshot);
    assert!(!after.contains(&late), "{after}");
    Ok(())
}

#[test]
fn a_same_attempt_result_of_a_superseded_execution_is_applied_without_routing() -> R {
    let mut rig = rig()?;
    let registry = job_registry();
    rig.tick_run(&registry)?;
    let job = rig.job_id();
    let current = rig
        .store
        .execution_by_job(&job)?
        .expect("the enqueued job has an execution");

    // A correction of the same attempt mints the next generation and
    // supersedes the execution the finished job belongs to. The attempt is
    // still the newest, so only the execution check can refuse the result.
    let next = rig.store.start_correction(&current.execution)?;
    assert_eq!(next.attempt, current.attempt);
    assert_eq!(current.attempt, Some(1));
    assert_eq!(rig.store.last_attempt(&rig.step)?, 1);

    rig.finish(&job)?;
    rig.tick_run(&registry)?;

    let text = rig.exceptions();
    assert!(text.contains(STALE_EXECUTION), "{text}");
    assert!(!text.contains("no collector for node"), "{text}");
    assert!(rig.jobs.jobs[&job].applied_at.is_some());
    assert_eq!(rig.status(&rig.step)?, TaskStatus::Open);
    assert_eq!(
        rig.store.step_by_task(&rig.step)?.expect("step row").state,
        StepState::Running
    );
    Ok(())
}

#[test]
fn a_pre_change_job_without_an_execution_keeps_the_attempt_check() -> R {
    let mut rig = rig()?;
    // A job enqueued before executions existed: a step_jobs row at attempt 1
    // and a finished result, beside a newer attempt 2 row and no execution.
    let old_id = format!("step-{}-a1", rig.step);
    for (id, attempt) in [(&old_id, 1), (&format!("step-{}-a2", rig.step), 2)] {
        rig.store.insert_step_job(
            &rig.need,
            &StepJobRow {
                job: id.to_owned(),
                run: 1,
                node: "plan".into(),
                task: rig.step.clone(),
                attempt,
            },
        )?;
    }
    let mut job = PoolJob::from(JobSpec::new(
        old_id.clone(),
        "planner",
        "planner",
        "subject",
        "body",
    ));
    job.status = JobStatus::Finished;
    rig.jobs.jobs.insert(old_id.clone(), job);

    rig.tick()?;

    let text = rig.exceptions();
    assert!(text.contains(STALE_ATTEMPT), "{text}");
    assert!(!text.contains(STALE_EXECUTION), "{text}");
    assert!(rig.jobs.jobs[&old_id].applied_at.is_some());
    Ok(())
}
