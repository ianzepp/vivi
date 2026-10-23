//! Tests for [`crate::pump::flow::advance::guard`] over the fake mail port.

use std::fs;

use tempfile::tempdir;

use super::pass;
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{SENTINEL_MARKER, STEP_MARKER};
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, NeedStatus, TaskStatus};
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const PUMP: &str = "pump";
const NEED: &str = "n1";

struct Fixture {
    _dir: tempfile::TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    run: RunRow,
    sentinel: String,
    step: String,
}

/// A live run with an open sentinel and one open step, bound to the need.
fn fixture() -> Result<Fixture, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new(PUMP);
    let sentinel = mail.send_step(PUMP, PUMP, "run", &format!("{SENTINEL_MARKER} n1 r1"), &[])?;
    let step = mail.send_step(
        PUMP,
        "hand",
        "plan",
        &format!("{STEP_MARKER} n1 r1 plan"),
        &[],
    )?;
    mail.bind(NEED, &[sentinel.clone(), step.clone()])?;
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
    for (node, task) in [("sentinel", &sentinel), ("plan", &step)] {
        store.insert_step(
            NEED,
            &StepRow {
                run: 1,
                node: node.into(),
                task: task.clone(),
                state: StepState::Running,
                verdict: None,
            },
        )?;
    }
    Ok(Fixture {
        _dir: dir,
        ctx,
        store,
        mail,
        run,
        sentinel,
        step,
    })
}

fn guard(f: &mut Fixture) -> R {
    pass(&f.ctx, &f.store, &mut f.mail, &f.run, PUMP)
}

fn exceptions(f: &Fixture) -> String {
    fs::read_to_string(f.ctx.home.join("exceptions.txt")).unwrap_or_default()
}

fn reopens(f: &Fixture) -> usize {
    f.mail
        .calls
        .iter()
        .filter(|c| matches!(c, Call::ReopenTask { .. }))
        .count()
}

#[test]
fn a_step_closed_by_mind_is_reopened_and_reported() -> R {
    let mut f = fixture()?;
    f.mail.foreign_close(&f.step, "mind");
    assert_eq!(f.mail.need_state(NEED)?, NeedStatus::Open);
    guard(&mut f)?;
    assert_eq!(f.mail.task_state(&f.step)?.status, TaskStatus::Open);
    assert_eq!(f.mail.task_state(&f.sentinel)?.status, TaskStatus::Open);
    let text = exceptions(&f);
    assert!(text.contains("flow-foreign-close"), "{text}");
    assert!(text.contains(&f.step) && text.contains("mind"), "{text}");
    Ok(())
}

#[test]
fn a_sentinel_closed_by_hand_is_reopened_and_the_need_restored() -> R {
    let mut f = fixture()?;
    f.mail.foreign_close(&f.step, "mind");
    f.mail.foreign_close(&f.sentinel, "mind");
    assert_eq!(f.mail.need_state(NEED)?, NeedStatus::Done);
    guard(&mut f)?;
    assert_eq!(f.mail.need_state(NEED)?, NeedStatus::Open);
    assert_eq!(f.mail.task_state(&f.sentinel)?.status, TaskStatus::Open);
    assert_eq!(f.mail.task_state(&f.step)?.status, TaskStatus::Open);
    assert_eq!(exceptions(&f).matches("flow-foreign-close").count(), 2);
    Ok(())
}

#[test]
fn a_closed_need_is_never_touched() -> R {
    let mut f = fixture()?;
    f.mail.foreign_close(&f.step, "mind");
    f.mail.close_need_by(NEED, "mind");
    guard(&mut f)?;
    assert_eq!(reopens(&f), 0);
    assert_eq!(f.mail.need_state(NEED)?, NeedStatus::Done);
    assert!(exceptions(&f).is_empty());
    Ok(())
}

#[test]
fn closes_the_pump_made_or_the_index_holds_are_not_foreign() -> R {
    let mut f = fixture()?;
    f.mail.close_task(&f.step, "built", None, None)?;
    f.store
        .set_step_state(&f.sentinel, StepState::Running, Some("done"))?;
    f.mail.foreign_close(&f.sentinel, "synced");
    let before = f.mail.calls.len();
    guard(&mut f)?;
    assert_eq!(f.mail.calls.len(), before);
    assert!(exceptions(&f).is_empty());
    Ok(())
}

#[test]
fn a_finished_step_is_left_alone() -> R {
    let mut f = fixture()?;
    f.mail.foreign_close(&f.step, "mind");
    f.store
        .set_step_state(&f.step, StepState::Done, Some("done"))?;
    guard(&mut f)?;
    assert_eq!(reopens(&f), 0);
    Ok(())
}
