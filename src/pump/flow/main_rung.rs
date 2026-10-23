//! Per-target main rung tick phase: a red run files or replies on the repair need, a green run records the last green tip.

mod green;
mod red;
mod schedule;

use std::collections::BTreeSet;

use crate::error::VivariumError;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::load::load;
use crate::pump::flow::rung_step;
use crate::pump::flow::tick::raise_once;
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobStore, PoolJob};
use crate::pump::store::PumpStore;

/// main_state key: the last main tip a main rung finished on.
pub(crate) const TESTED: &str = "main-tested";
/// main_state key: the last main tip whose main rung passed.
pub(crate) const GREEN: &str = "main-green";
/// main_state key: unix seconds when the last main rung job was started.
pub(crate) const STARTED: &str = "main-started";
/// main_state key: the failing test id set of the last red main rung.
pub(crate) const FAILING: &str = "main-failing";
/// main_state key: the long-lived task the main rung jobs run on.
pub(crate) const TASK: &str = "main-rung-task";
/// Job payload key holding the target a main rung job tests.
pub(crate) const PAYLOAD_KEY: &str = "main_rung";

/// A per-target main_state key, `<name>:<target>`.
pub(crate) fn key(name: &str, target: &str) -> String {
    format!("{name}:{target}")
}

/// The trimmed value of the first `target:` line of a record body; None when absent or empty.
pub(crate) fn record_target(body: &str) -> Option<&str> {
    body.lines()
        .find_map(|line| line.trim().strip_prefix("target:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Exception kind of a main rung job that ended without an exit code.
pub(crate) const MAIN_RUNG_FAILED: &str = "flow-main-rung-failed";

/// The last main tip whose main rung passed on `target`, for status.
///
/// # Errors
/// Returns a [`VivariumError`] when the state cannot be read.
pub fn last_green(store: &PumpStore, target: &str) -> Result<Option<String>, VivariumError> {
    store.main_state(&key(GREEN, target))
}

struct MainRung;

impl<S: JobStore, M: FlowMail> TickPhase<S, M> for MainRung {
    fn name(&self) -> &'static str {
        "main-rung"
    }

    fn run(
        &self,
        ctx: &Ctx,
        st: &mut S,
        store: &PumpStore,
        mail: &mut M,
    ) -> Result<(), VivariumError> {
        for target in ctx.cfg.target_configs().keys() {
            if ctx
                .cfg
                .effective_rungs(Some(target))
                .command("main")
                .is_none()
            {
                continue;
            }
            collect(ctx, st, store, mail, target)?;
            schedule::schedule(ctx, st, store, mail, target)?;
        }
        Ok(())
    }
}

/// Apply every terminal, unapplied main rung job of `target`.
fn collect<S: JobStore, M: FlowMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &mut M,
    target: &str,
) -> Result<(), VivariumError> {
    let finished: Vec<PoolJob> = st
        .jobs()
        .values()
        .filter(|job| {
            job.kind == "rung"
                && job.status.is_terminal()
                && job.applied_at.is_none()
                && job.payload.get(PAYLOAD_KEY).and_then(|v| v.as_str()) == Some(target)
        })
        .cloned()
        .collect();
    for job in finished {
        apply(ctx, store, mail, target, &job)?;
        jobs::mark_applied(st, &job.id, ctx.now());
    }
    Ok(())
}

/// Record one finished main rung job.
fn apply<M: FlowMail>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    target: &str,
    job: &PoolJob,
) -> Result<(), VivariumError> {
    let tip = job
        .payload
        .get("tip")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (word, payload) = rung_step::answer(job);
    match word {
        "pass" => {
            store.set_main_state(&key(TESTED, target), tip)?;
            store.set_main_state(&key(GREEN, target), tip)?;
            store.clear_main_state(&key(FAILING, target))?;
            match load(FlowKind::Repair) {
                Ok(def) => {
                    green::answer_green(ctx, store, mail, &def, target, tip)?;
                }
                Err(e) => ctx.log(&format!("main rung green: repair flow not shipped: {e}")),
            }
        }
        "fail" => {
            store.set_main_state(&key(TESTED, target), tip)?;
            let failing: BTreeSet<String> = payload
                .get("failing")
                .and_then(|v| v.as_array())
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let tail = payload.get("tail").and_then(|v| v.as_str()).unwrap_or("");
            red::report_red(ctx, store, mail, target, tip, &failing, tail)?;
        }
        _ => raise_once(
            ctx,
            MAIN_RUNG_FAILED,
            &job.id,
            &format!(
                "main rung job {} for target {target} at {tip} ended {:?} with no exit code; the tip runs again after the interval",
                job.id, job.status
            ),
        ),
    }
    Ok(())
}

/// Register the main rung tick phase.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.phase(Box::new(MainRung));
}

#[cfg(test)]
#[path = "main_rung_test.rs"]
mod tests;
