//! The completion guard on a real mailspace (RC3 test 1): foreign closes by
//! `mind` and by `graph audit --repair`.
//!
//! One-pass window (plan 3.4(a), choice C6): while the run's sentinel is
//! open, a hand-closed step cannot close the need. Once the sentinel itself
//! is closed by hand, the need closes as soon as every bound unit is done,
//! and stays closed until the next [`pass`]. That pass is the only repair
//! for it, so the need is closed for at most one tick. The pass then reopens
//! both tasks and the need comes back.

use super::{FOREIGN_CLOSE, pass};
use crate::error::VivariumError;
use crate::letter::fixture::{sentinel_body, step_body};
use crate::mailspace::RoleUpdate;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::start::SENTINEL_NODE;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailspacePort, NeedStatus, TaskStatus};
use crate::pump::host::Ctx;
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

/// The pump's mailspace identity in the fixture (`[pump] from`).
const PUMP: &str = "operator";

struct Fixture {
    p: Project,
    ctx: Ctx,
    store: PumpStore,
    port: MailspacePort,
    run: RunRow,
    need: String,
    sentinel: String,
    step: String,
}

/// A live run, as `run::start` writes it but with the step tasks sent to
/// `mind`: the sentinel and one step, bound to the need, rows in the index.
fn fixture() -> Result<Fixture, VivariumError> {
    let mut p = project();
    p.mailspace.set_role(
        "mind",
        RoleUpdate {
            kind: Some(Some("mind".into())),
            ..RoleUpdate::default()
        },
    )?;
    let ctx = p.pump_ctx();
    let store = PumpStore::open(&p.home().join(DB_FILENAME))?;
    let port = MailspacePort {
        mailspace: p.mailspace.clone(),
        pump_identity: PUMP.into(),
    };
    let need = p.need("guard a need");
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
    let run = RunRow {
        need: need.clone(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    };
    store.insert_run(&run)?;
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
    Ok(Fixture {
        p,
        ctx,
        store,
        port,
        run,
        need,
        sentinel,
        step,
    })
}

impl Fixture {
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
fn mind_then_audit_repair_closes_are_reopened_and_reported() -> R {
    let mut f = fixture()?;

    f.mind_closes(&f.step)?;
    assert_eq!(f.status(&f.step)?, TaskStatus::Done);
    assert_eq!(f.port.need_state(&f.need)?, NeedStatus::Open);

    f.mind_closes(&f.sentinel)?;
    f.p.mailspace.backlog_audit(true)?;
    assert_eq!(f.port.need_state(&f.need)?, NeedStatus::Done);

    pass(&f.ctx, &f.store, &mut f.port, &f.run, PUMP)?;

    assert_eq!(f.status(&f.step)?, TaskStatus::Open);
    assert_eq!(f.status(&f.sentinel)?, TaskStatus::Open);
    assert_eq!(f.port.need_state(&f.need)?, NeedStatus::Open);
    let text = f.exceptions();
    assert_eq!(text.matches(FOREIGN_CLOSE).count(), 2, "{text}");
    Ok(())
}
