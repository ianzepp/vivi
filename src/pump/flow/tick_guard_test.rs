//! RC3 test 1 through the tick: the join-close case and the withdrawal case,
//! on a real mailspace.
//!
//! A foreign close of the sentinel and the last open step lets the join close
//! the need. The next tick must reopen both tasks and the need (recording
//! `flow-foreign-close`) and leave the run live, not withdraw it. A need
//! closed while a run task is still open stays a withdrawal.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::{HAND_PHASE, OPERATOR_HOLD, tick_with};
use crate::error::VivariumError;
use crate::letter::fixture::{record_body, sentinel_body, step_body};
use crate::mailspace::RoleUpdate;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::guard::FOREIGN_CLOSE;
use crate::pump::flow::advance::start::SENTINEL_NODE;
use crate::pump::flow::collect_step::{RunCx, StepCollector};
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, MailspacePort, NeedStatus, TaskStatus};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler, role_job};
use crate::pump::flow::recovery_inputs;
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::step_execution::{ExecutionKey, ExecutionRow};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;
type Reg = Registry<Jobs, MailspacePort>;
type Collected = Arc<Mutex<Vec<String>>>;

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

    fn tick_registry(&mut self, registry: &Reg) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.port,
            registry,
        )
    }

    /// A reply on the need's thread from `from`.
    fn reply(&self, from: &str, body: &str) -> R {
        self.p.mailspace.reply(
            &self.need,
            from,
            vec![PUMP.into()],
            Vec::new(),
            None,
            body.to_owned(),
        )?;
        Ok(())
    }

    /// The active `hand` execution of the rig's step.
    fn execution(&self) -> Result<ExecutionRow, VivariumError> {
        let key = ExecutionKey::new(&self.need, 1, &self.step, HAND_PHASE);
        self.store
            .active_execution(&key)?
            .ok_or_else(|| VivariumError::Message("no active execution".into()))
    }

    /// A finished, unapplied pool job.
    fn finish(&mut self, id: &str) {
        let mut job = PoolJob::from(JobSpec::new(id, "planner", "planner", "s", "b"));
        job.status = JobStatus::Finished;
        self.jobs.jobs.insert(id.to_owned(), job);
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

/// A handler that answers its start with a planner job.
struct Enqueuing;

impl StepHandler for Enqueuing {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::Job(Box::new(role_job(
            cx.ctx,
            cx.run,
            step,
            "planner",
            RoleKind::Planner,
            "planner",
            "brief",
        ))))
    }
}

/// A collector that records the jobs it was handed and answers nothing.
struct Collecting {
    collected: Collected,
}

impl StepCollector<Jobs, MailspacePort> for Collecting {
    fn collect(
        &self,
        _cx: &mut RunCx<'_, Jobs, MailspacePort>,
        job: &PoolJob,
        _row: &StepJobRow,
        _step: &StepRow,
        _class: &NodeClass,
    ) -> R {
        self.collected
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(job.id.clone());
        Ok(())
    }
}

#[test]
fn an_enqueued_handler_job_activates_its_execution_with_its_input() -> R {
    let mut rig = rig()?;
    rig.reply("elsewhere", "narrow the scope to the tick")?;
    let mut registry = Reg::new();
    registry.handler(FlowKind::Build, NodeClass::Planner, Box::new(Enqueuing));

    rig.tick_registry(&registry)?;

    let execution = rig.execution()?;
    assert_eq!(execution.phase, HAND_PHASE);
    assert_eq!(execution.generation, 1);
    assert_eq!(
        execution.execution,
        format!("{}:r1:{}:hand:g1", rig.need, rig.step)
    );
    let job = format!("step-{}-a1", rig.step);
    assert_eq!(execution.active_job.as_deref(), Some(job.as_str()));
    assert_eq!(execution.attempt, Some(1));
    let ids: Vec<String> = serde_json::from_str(
        execution
            .input_checkpoint
            .as_deref()
            .expect("input checkpoint"),
    )
    .map_err(|e| VivariumError::Other(e.to_string()))?;
    let eligible = recovery_inputs::eligible(&rig.p.mailspace, PUMP, &rig.need)?;
    assert_eq!(ids, eligible);
    assert_eq!(ids.len(), 1, "{ids:?}");
    Ok(())
}

#[test]
fn a_same_attempt_stale_result_is_marked_applied_without_routing() -> R {
    let mut rig = rig()?;
    let collected: Collected = Arc::new(Mutex::new(Vec::new()));
    let mut registry = Reg::new();
    registry.handler(FlowKind::Build, NodeClass::Planner, Box::new(Enqueuing));
    registry.collector(
        FlowKind::Build,
        NodeClass::Planner,
        Box::new(Collecting {
            collected: Arc::clone(&collected),
        }),
    );
    rig.tick_registry(&registry)?;
    let id = format!("step-{}-a1", rig.step);

    // A correction of the same attempt takes the task over first.
    let execution = rig.execution()?;
    let correction = rig.store.start_correction(&execution.execution)?;
    assert_eq!(correction.attempt, execution.attempt);

    rig.finish(&id);
    rig.tick_registry(&registry)?;

    assert!(
        collected
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "the stale result never routed"
    );
    let job = rig.jobs.jobs.get(&id).expect("job");
    assert!(job.applied_at.is_some());
    let step = rig.store.step_by_task(&rig.step)?.expect("step");
    assert_eq!(step.state, StepState::Running);
    assert_eq!(step.verdict, None);
    assert!(rig.exceptions().contains("flow-stale-execution"));
    Ok(())
}

#[test]
fn a_pre_change_result_is_adopted_and_applies() -> R {
    let mut rig = rig()?;
    let collected: Collected = Arc::new(Mutex::new(Vec::new()));
    let mut registry = Reg::new();
    registry.collector(
        FlowKind::Build,
        NodeClass::Planner,
        Box::new(Collecting {
            collected: Arc::clone(&collected),
        }),
    );
    // A job recorded before executions existed: a step_jobs row and a
    // finished pool job, with no execution registered for the task.
    let id = String::from("step-old-a1");
    rig.store.insert_step_job(
        &rig.need,
        &StepJobRow {
            job: id.clone(),
            run: 1,
            node: "plan".into(),
            task: rig.step.clone(),
            attempt: 1,
        },
    )?;
    rig.finish(&id);

    rig.tick_registry(&registry)?;

    assert_eq!(
        *collected
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![id.clone()]
    );
    let job = rig.jobs.jobs.get(&id).expect("job");
    assert!(job.applied_at.is_some());
    let execution = rig.execution()?;
    assert_eq!(execution.active_job.as_deref(), Some(id.as_str()));
    assert_eq!(
        execution.attempt,
        Some(1),
        "the attempt adopted from step_jobs"
    );
    Ok(())
}

#[test]
fn a_reply_after_the_capture_is_not_in_the_parked_execution_s_input() -> R {
    let mut rig = rig()?;
    rig.reply("elsewhere", "before the capture")?;
    let mut registry = Reg::new();
    registry.handler(FlowKind::Build, NodeClass::Planner, Box::new(Enqueuing));
    rig.tick_registry(&registry)?;
    let execution = rig.execution()?;
    let captured = execution
        .input_checkpoint
        .clone()
        .expect("input checkpoint");

    // The reply arrives after the enqueue but before the step parks.
    rig.reply("elsewhere", "after the capture")?;
    rig.store
        .set_step_state(&rig.step, StepState::Waiting, Some(OPERATOR_HOLD))?;
    rig.tick_registry(&registry)?;

    let parked = rig.execution()?;
    assert_eq!(parked.input_checkpoint.as_deref(), Some(captured.as_str()));
    let ids: Vec<String> = serde_json::from_str(
        parked
            .input_checkpoint
            .as_deref()
            .expect("input checkpoint"),
    )
    .map_err(|e| VivariumError::Other(e.to_string()))?;
    let after = recovery_inputs::eligible(&rig.p.mailspace, PUMP, &rig.need)?;
    assert_eq!(after.len(), ids.len() + 1, "{after:?}");
    Ok(())
}
