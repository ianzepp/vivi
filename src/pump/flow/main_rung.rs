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
use crate::pump::jobs::rung_result::{self, EnvFailure};
use crate::pump::jobs::{self, JobStore, PoolJob};
use crate::pump::store::PumpStore;

/// main_state key: the last main tip a main rung finished on.
pub(crate) const TESTED: &str = "main-tested";
/// main_state key: the last tip whose main rung hit an environment failure.
pub(crate) const ENV_FAILED: &str = "main-env-failed";
/// main_state key: the last main tip whose main rung passed.
pub(crate) const GREEN: &str = "main-green";
/// main_state key: unix seconds when the last main rung job was started.
pub(crate) const STARTED: &str = "main-started";
/// main_state key: the failing test id set of the last red main rung.
pub(crate) const FAILING: &str = "main-failing";
/// main_state key: the main sha of the newest landed repair merge.
pub(crate) const REPAIRED: &str = "main-repaired";
/// main_state key: the long-lived task the main rung jobs run on.
pub(crate) const TASK: &str = "main-rung-task";
/// main_state key: `<tip> <count>`, the lost main rung jobs seen on that tip.
pub(crate) const LOST: &str = "main-lost";
/// Lost, timed-out or setup-failed main rung jobs one tip may take before it counts as tested.
pub(crate) const MAX_LOST_PER_TIP: u32 = 2;
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
/// Exception kind of a finished main rung job whose target has no main rung configured.
pub(crate) const MAIN_TARGET_UNKNOWN: &str = "flow-main-target-unknown";

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
        let mut configured = Vec::new();
        for target in ctx.cfg.target_configs().keys() {
            if ctx
                .cfg
                .effective_rungs(Some(target))
                .command("main")
                .is_none()
            {
                continue;
            }
            configured.push(target.clone());
            collect(ctx, st, store, mail, target)?;
            schedule::schedule(ctx, st, store, mail, target)?;
        }
        retire_unknown(ctx, st, &configured);
        Ok(())
    }
}

/// Raise once and mark applied every finished, unapplied main rung job whose
/// target is not among the `configured` main rung targets (removed or renamed).
fn retire_unknown<S: JobStore>(ctx: &Ctx, st: &mut S, configured: &[String]) {
    let orphans: Vec<(String, String)> = st
        .jobs()
        .values()
        .filter(|job| job.kind == "rung" && job.status.is_terminal() && job.applied_at.is_none())
        .filter_map(|job| {
            let target = job.payload.get(PAYLOAD_KEY)?.as_str()?;
            (!configured.iter().any(|t| t == target)).then(|| (job.id.clone(), target.to_owned()))
        })
        .collect();
    for (id, target) in orphans {
        raise_once(
            ctx,
            MAIN_TARGET_UNKNOWN,
            &id,
            &format!(
                "main rung job {id}: expected target {target} configured with a main rung; actual: configured [{}]",
                configured.join(", ")
            ),
        );
        jobs::mark_applied(st, &id, ctx.now());
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
        let same_tip = same_tip_ids(st, target, &job);
        apply(ctx, store, mail, target, &job, &same_tip)?;
        jobs::mark_applied(st, &job.id, ctx.now());
    }
    Ok(())
}

/// Ids of every main rung job of `target` that ran at the same tip as `job`, sorted.
fn same_tip_ids<S: JobStore>(st: &S, target: &str, job: &PoolJob) -> Vec<String> {
    let tip = job.payload.get("tip");
    let mut ids: Vec<String> = st
        .jobs()
        .values()
        .filter(|other| {
            other.kind == "rung"
                && other.payload.get(PAYLOAD_KEY).and_then(|v| v.as_str()) == Some(target)
                && other.payload.get("tip") == tip
        })
        .map(|other| other.id.clone())
        .collect();
    ids.sort();
    ids
}

/// An environment failure is not a red main: no repair need, one alarm per
/// (target, tip, tool). The tip is recorded as env-failed (not tested, not
/// green, not red) and does not run again; a new tip runs after the interval.
fn env_hold(
    ctx: &Ctx,
    store: &PumpStore,
    target: &str,
    tip: &str,
    job: &PoolJob,
    env: &EnvFailure,
) -> Result<(), VivariumError> {
    store.set_main_state(&key(ENV_FAILED, target), tip)?;
    let tool = env.tool.as_deref().unwrap_or("host");
    raise_once(
        ctx,
        rung_result::ENV_FAILURE,
        &format!("{target}:{tip}:{tool}"),
        &format!(
            "main rung for target {target} at {tip}: expected the rung script to run; actual: {:?}, tool {tool}: {} (job {}, pool {}). No repair need filed; this tip is not run again; a new main tip runs after the interval",
            env.kind,
            env.evidence,
            job.id,
            job.pool.as_deref().unwrap_or("none"),
        ),
    );
    Ok(())
}

/// The lost count stored for `tip`; 0 when absent or stored for another tip.
///
/// # Errors
/// A store failure, or a stored value for `tip` whose count does not parse.
pub(crate) fn lost_count(store: &PumpStore, target: &str, tip: &str) -> Result<u32, VivariumError> {
    let name = key(LOST, target);
    let Some(text) = store.main_state(&name)? else {
        return Ok(0);
    };
    match text.split_once(' ') {
        Some((seen, count)) if seen == tip => count.parse::<u32>().map_err(|e| {
            VivariumError::Parse(format!("main_state {name} value {text:?}: bad count: {e}"))
        }),
        _ => Ok(0),
    }
}

/// A main rung job that ended without an exit code: count it against the tip.
/// At [`MAX_LOST_PER_TIP`] the tip counts as tested; below it the schedule
/// runs the tip once more after the interval.
fn lost(
    ctx: &Ctx,
    store: &PumpStore,
    target: &str,
    tip: &str,
    job: &PoolJob,
    same_tip: &[String],
) -> Result<(), VivariumError> {
    let count = lost_count(store, target, tip)? + 1;
    store.set_main_state(&key(LOST, target), &format!("{tip} {count}"))?;
    if count >= MAX_LOST_PER_TIP {
        store.set_main_state(&key(TESTED, target), tip)?;
        raise_once(
            ctx,
            MAIN_RUNG_FAILED,
            &job.id,
            &format!(
                "main rung for target {target} at {tip} ended {:?} with no exit code {count} times (jobs {}); the tip is marked tested and will not run again",
                job.status,
                same_tip.join(", ")
            ),
        );
    } else {
        raise_once(
            ctx,
            MAIN_RUNG_FAILED,
            &job.id,
            &format!(
                "main rung job {} for target {target} at {tip} ended {:?} with no exit code; the tip runs again after the interval",
                job.id, job.status
            ),
        );
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
    same_tip: &[String],
) -> Result<(), VivariumError> {
    let tip = job
        .payload
        .get("tip")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if let Some(env) = rung_result::env_failure(job) {
        return env_hold(ctx, store, target, tip, job, &env);
    }
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
        _ => lost(ctx, store, target, tip, job, same_tip)?,
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
