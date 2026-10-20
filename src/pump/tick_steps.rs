//! Tick steps that may fail without ending the pump.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::VivariumError;
use crate::pump::host::Ctx;
use crate::pump::state::State;

/// A step that keeps failing re-raises its exception at most this often while
/// the error text stays the same; a changed error is raised at once.
pub(crate) const RERAISE_SECS: u64 = 600;

/// Run one fallible tick step. A failure (one failed `git ls-remote`, one bad
/// need row) must not end the pump, because the service would only restart it
/// into the same failure: it is logged, raised as a `tick-step-failed`
/// operator exception naming the step, and the tick goes on. Saving the state
/// is not a step: a failed save still ends the run.
pub(crate) fn soft_step<T>(ctx: &Ctx, step: &str, result: Result<T, VivariumError>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            let text = error.to_string();
            ctx.log(&format!("tick step {step} failed: {text}"));
            let key = format!("{}\u{0}{step}", ctx.home.display());
            if should_raise(&key, &text, ctx.now()) {
                ctx.exception("tick-step-failed", step, &text);
            }
            None
        }
    }
}

/// Whether a failure keyed by `key` (pump home and step) with `text` at `now`
/// is new enough to raise: the first failure, a changed error, or the same
/// error after [`RERAISE_SECS`]. The record lives for the pump process.
pub(crate) fn should_raise(key: &str, text: &str, now: u64) -> bool {
    static LAST: OnceLock<Mutex<HashMap<String, (String, u64)>>> = OnceLock::new();
    let mut last = LAST
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let fresh = last
        .get(key)
        .is_none_or(|(seen, at)| seen != text || now.saturating_sub(*at) >= RERAISE_SECS);
    if fresh {
        last.insert(key.to_owned(), (text.to_owned(), now));
    }
    fresh
}

/// Queue the need pipeline's audit, landing and merge jobs.
pub(super) fn need_dispatch_phase(ctx: &Ctx, st: &mut State) {
    soft_step(
        ctx,
        "dispatch-audits",
        crate::pump::need_pipeline::dispatch_audits(ctx, st),
    );
    soft_step(
        ctx,
        "dispatch-landings",
        crate::pump::need_landing::dispatch(ctx, st),
    );
    soft_step(
        ctx,
        "dispatch-merges",
        crate::pump::merge::dispatch_pool(ctx, st),
    );
}

/// Fold finished need audits, landings and freezes back into the state.
pub(super) fn need_collect_phase(ctx: &Ctx, st: &mut State) {
    soft_step(
        ctx,
        "refresh-freezes",
        crate::pump::merge::refresh_need_freezes(ctx, st),
    );
    soft_step(
        ctx,
        "collect-audits",
        crate::pump::need_pipeline::collect_audits(ctx, st),
    );
    soft_step(
        ctx,
        "collect-landings",
        crate::pump::need_landing::collect_completed(ctx, st),
    );
    soft_step(
        ctx,
        "refresh-default-merges",
        crate::pump::need_pipeline::refresh_default_merges(st),
    );
}
