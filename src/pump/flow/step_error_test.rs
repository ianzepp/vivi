//! The consecutive-failure counter and the operator hold.

use std::fs;

use tempfile::tempdir;

use super::{STEP_ERROR_HELD, hold, note_failure, note_success};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::tick::OPERATOR_HOLD;
use crate::pump::host::test_ctx;
use crate::pump::store::{DB_FILENAME, PumpStore};

#[test]
fn counts_go_up_and_reset_on_success() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    assert_eq!(note_failure(&ctx, "a"), 1);
    assert_eq!(note_failure(&ctx, "a"), 2);
    assert_eq!(note_failure(&ctx, "a"), 3);
    note_success(&ctx, "a");
    assert_eq!(note_failure(&ctx, "a"), 1);
}

#[test]
fn count_restarts_after_the_limit() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    assert_eq!(note_failure(&ctx, "a"), 1);
    assert_eq!(note_failure(&ctx, "a"), 2);
    assert_eq!(note_failure(&ctx, "a"), 3);
    assert_eq!(note_failure(&ctx, "a"), 1);
    note_success(&ctx, "a");
    assert_eq!(note_failure(&ctx, "a"), 1);
}

#[test]
fn keys_count_apart() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    assert_eq!(note_failure(&ctx, "x"), 1);
    assert_eq!(note_failure(&ctx, "x"), 2);
    assert_eq!(note_failure(&ctx, "y"), 1);
}

#[test]
fn homes_count_apart() {
    let one = tempdir().unwrap();
    let two = tempdir().unwrap();
    let (first, _h1) = test_ctx(one.path(), PumpConfig::default());
    let (second, _h2) = test_ctx(two.path(), PumpConfig::default());
    assert_eq!(note_failure(&first, "k"), 1);
    assert_eq!(note_failure(&first, "k"), 2);
    assert_eq!(note_failure(&second, "k"), 1);
}

#[test]
fn hold_sets_operator_hold_and_raises_once() -> Result<(), VivariumError> {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let row = StepRow {
        run: 1,
        node: "plan".into(),
        task: "task-1".into(),
        state: StepState::Running,
        verdict: None,
    };
    store.insert_step("n1", &row)?;
    hold(&ctx, &store, "task-1", "boom")?;
    hold(&ctx, &store, "task-1", "boom")?;
    let held = store.step_by_task("task-1")?.expect("step row");
    assert_eq!(held.state, StepState::Waiting);
    assert_eq!(held.verdict.as_deref(), Some(OPERATOR_HOLD));
    let text = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert_eq!(text.matches(STEP_ERROR_HELD).count(), 1, "{text}");
    Ok(())
}
