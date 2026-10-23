//! Tests for the `thread-change` phase, on a real mailspace: a reply by another
//! member after the run's start post restarts a live or parked run at `plan`,
//! raises once in a locked run, and a reply by the pump changes nothing.

use std::thread::sleep;
use std::time::Duration;

use super::{THREAD_CHANGED, ThreadChange};
use crate::error::VivariumError;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::handlers::TickPhase;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::{MailWriter, MailspacePort};
use crate::pump::host::Ctx;
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const PUMP: &str = "mind";

struct Rig {
    _project: Project,
    ctx: Ctx,
    store: PumpStore,
    port: MailspacePort,
    need: String,
}

/// A need with an open pump record and a build run started at `plan`.
fn rig() -> Result<Rig, VivariumError> {
    let p = project();
    let ctx = p.pump_ctx();
    let need = p.need("change me");
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut port = MailspacePort {
        mailspace: p.mailspace.clone(),
        pump_identity: PUMP.into(),
    };
    MailWriter::open_record(&mut port, &need, "kind: build")?;
    start(&ctx, &store, &mut port, &need, Some("plan"))?;
    Ok(Rig {
        _project: p,
        ctx,
        store,
        port,
        need,
    })
}

impl Rig {
    /// A reply on the need's thread, one second after the run's start post.
    fn reply_as(&self, from: &str) -> R {
        sleep(Duration::from_millis(1100));
        let to = if from == PUMP { "operator" } else { PUMP };
        self.port.mailspace.reply(
            &self.need,
            from,
            vec![to.into()],
            Vec::new(),
            None,
            "actually, do it differently".into(),
        )?;
        Ok(())
    }

    fn phase(&mut self) -> R {
        ThreadChange.run(&self.ctx, &mut (), &self.store, &mut self.port)
    }

    fn runs(&self) -> Result<Vec<(u32, String, String)>, VivariumError> {
        Ok(self
            .store
            .runs_of(&self.need)?
            .into_iter()
            .map(|run| (run.run, run.entry, run.state))
            .collect())
    }

    /// A waiting step row for `node` on run 1, over a real task.
    fn hold(&mut self, node: &str) -> R {
        let task = self.port.send_step(
            PUMP,
            PUMP,
            node,
            &format!("pump-step: {} r1 {node}", self.need),
            &[],
        )?;
        self.store.insert_step(
            &self.need,
            &StepRow {
                run: 1,
                node: node.into(),
                task,
                state: StepState::Waiting,
                verdict: None,
            },
        )
    }

    fn exceptions(&self) -> String {
        std::fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }
}

#[test]
fn a_reply_by_another_member_restarts_a_live_run_at_plan() -> R {
    let mut rig = rig()?;
    rig.reply_as("operator")?;
    rig.phase()?;
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0], (1, "plan".into(), "superseded".into()));
    assert_eq!(runs[1], (2, "plan".into(), "live".into()));
    assert_eq!(rig.exceptions(), "");
    // The next pass sees the reply as older than run 2's start: no third run.
    rig.phase()?;
    assert_eq!(rig.runs()?.len(), 2);
    Ok(())
}

#[test]
fn a_reply_by_the_pump_identity_changes_nothing() -> R {
    let mut rig = rig()?;
    rig.reply_as(PUMP)?;
    rig.phase()?;
    assert_eq!(rig.runs()?, [(1, "plan".into(), "live".into())]);
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_parked_run_restarts_at_plan() -> R {
    let mut rig = rig()?;
    rig.hold("parked")?;
    rig.reply_as("operator")?;
    rig.phase()?;
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[1], (2, "plan".into(), "live".into()));
    Ok(())
}

#[test]
fn a_reply_in_a_locked_run_raises_once_and_starts_nothing() -> R {
    let mut rig = rig()?;
    rig.hold("locked")?;
    rig.reply_as("operator")?;
    rig.phase()?;
    rig.phase()?;
    assert_eq!(rig.runs()?, [(1, "plan".into(), "live".into())]);
    let raised = rig.exceptions();
    assert_eq!(raised.matches(THREAD_CHANGED).count(), 1, "{raised}");
    assert!(raised.contains(&rig.need), "{raised}");
    Ok(())
}

#[test]
fn a_reply_after_merge_main_started_raises_and_does_not_restart() -> R {
    let mut rig = rig()?;
    rig.hold("merge")?;
    rig.reply_as("operator")?;
    rig.phase()?;
    assert_eq!(rig.runs()?, [(1, "plan".into(), "live".into())]);
    assert_eq!(rig.exceptions().matches(THREAD_CHANGED).count(), 1);
    Ok(())
}
