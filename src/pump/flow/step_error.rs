//! Consecutive-failure counter for flow units; a unit that fails
//! [`STEP_ERROR_LIMIT`] ticks in a row is held for the operator.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::error::VivariumError;
use crate::pump::flow::index::StepState;
use crate::pump::flow::tick::{OPERATOR_HOLD, raise_once};
use crate::pump::host::Ctx;
use crate::pump::store::PumpStore;

/// Consecutive failing ticks after which a unit is held.
pub(crate) const STEP_ERROR_LIMIT: u32 = 3;

/// Exception kind raised when a unit is held after repeated failures.
pub(crate) const STEP_ERROR_HELD: &str = "flow-step-error-held";

fn counts() -> std::sync::MutexGuard<'static, HashMap<String, u32>> {
    static COUNTS: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    COUNTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scoped(ctx: &Ctx, key: &str) -> String {
    format!("{}\u{0}{key}", ctx.home.display())
}

/// Count one more consecutive failure of `key` in this process; returns the count.
pub(crate) fn note_failure(ctx: &Ctx, key: &str) -> u32 {
    let mut counts = counts();
    let count = counts.entry(scoped(ctx, key)).or_insert(0);
    *count += 1;
    *count
}

/// A success: the consecutive count of `key` goes back to zero.
pub(crate) fn note_success(ctx: &Ctx, key: &str) {
    counts().remove(&scoped(ctx, key));
}

/// Hold the step `task` for the operator and raise [`STEP_ERROR_HELD`] once.
pub(crate) fn hold(
    ctx: &Ctx,
    store: &PumpStore,
    task: &str,
    text: &str,
) -> Result<(), VivariumError> {
    store.set_step_state(task, StepState::Waiting, Some(OPERATOR_HOLD))?;
    raise_once(ctx, STEP_ERROR_HELD, task, text);
    Ok(())
}

#[cfg(test)]
#[path = "step_error_test.rs"]
mod tests;
