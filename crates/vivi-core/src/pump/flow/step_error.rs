//! Consecutive-failure counter for flow units; a unit that fails
//! [`STEP_ERROR_LIMIT`] ticks in a row, and for at least
//! [`STEP_ERROR_MIN_SECS`] of ticks, is held for the operator.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::pump::flow::index::StepState;
use crate::pump::flow::tick::{OPERATOR_HOLD, raise_once};
use crate::pump::host::Ctx;
use crate::pump::store::PumpStore;
use vivi_error::VivariumError;

/// Consecutive failing ticks after which a unit is held.
pub(crate) const STEP_ERROR_LIMIT: u32 = 3;

/// The least span of failing ticks before a hold. The limit counts ticks,
/// and a short `[pump] tick` must not turn a brief outage (a lane or worker
/// unreachable for half a minute) into holds the operator has to clear: at
/// the 20 s default tick, three failing ticks span this long.
pub(crate) const STEP_ERROR_MIN_SECS: u64 = 60;

/// Consecutive failing ticks before a hold under `tick`: the limit, or more
/// when ticks are short enough that the limit would span less than
/// [`STEP_ERROR_MIN_SECS`].
fn ticks_to_hold(tick: u64) -> u32 {
    let spanning = STEP_ERROR_MIN_SECS.div_ceil(tick.max(1));
    STEP_ERROR_LIMIT.max(u32::try_from(spanning).unwrap_or(u32::MAX))
}

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
///
/// Callers hold at [`STEP_ERROR_LIMIT`]. Under a short tick more failures are
/// needed ([`ticks_to_hold`]); until then the count returned stays below the
/// limit. When the failures needed are reached the true count is returned and
/// the key is removed in the same call, so the next failure of that key counts
/// from 1. A requeued unit therefore gets fresh tries after its hold. Each
/// caller holds or settles at the limit and does not call again for the same
/// item.
pub(crate) fn note_failure(ctx: &Ctx, key: &str) -> u32 {
    let needed = ticks_to_hold(ctx.cfg.tick);
    let mut counts = counts();
    let scoped_key = scoped(ctx, key);
    let count = counts.entry(scoped_key.clone()).or_insert(0);
    *count += 1;
    let n = *count;
    if n < needed {
        return n.min(STEP_ERROR_LIMIT - 1);
    }
    counts.remove(&scoped_key);
    n
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
