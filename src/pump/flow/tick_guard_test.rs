//! RC3 test 1 through the tick: the join-close case and the withdrawal case,
//! on a real mailspace.
//!
//! A foreign close of the sentinel and the last open step lets the join close
//! the need. The next tick must reopen both tasks and the need (recording
//! `flow-foreign-close`) and leave the run live, not withdraw it. A need
//! closed while a run task is still open stays a withdrawal.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::tick_with;
use crate::error::VivariumError;
use crate::mailspace::RoleUpdate;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::guard::FOREIGN_CLOSE;
use crate::pump::flow::def::{SENTINEL_MARKER, STEP_MARKER};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, MailspacePort, NeedStatus, TaskStatus};
use crate::pump::flow::outcome::StepHandlers;
use crate::pump::flow::run::SENTINEL_NODE;
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

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
    MailWriter::open_record(&mut port, &need, "kind: build")?;
    let sentinel = port.send_step(
        PUMP,
        "mind",
        "r1 sentinel",
        &format!("{SENTINEL_MARKER} {need} r1"),
        &[],
    )?;
    let step = port.send_step(
        PUMP,
        "mind",
        "r1 plan",
        &format!("{STEP_MARKER} {need} r1 plan"),
        &[],
    )?;
    port.bind(&need, &[sentinel.clone(), step.clone()])?;
    store.insert_run(&RunRow {
        need: need.clone(),
        run: 1,
        flow: "build".into(),
        flow_version: 1,
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
            &StepHandlers::new(),
        )
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
