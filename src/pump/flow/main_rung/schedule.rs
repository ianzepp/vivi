//! Starts a main rung job when main has a new tip.
//!
//! Per target: when the main tip differs from the last tested tip and the
//! interval has passed since the last start, one main rung job is enqueued on
//! the target's long-lived main rung task. The job is not a flow step, so it
//! has no `step_jobs` row.

use super::{ENV_FAILED, PAYLOAD_KEY, STARTED, TASK, TESTED, key};
use crate::error::VivariumError;
use crate::letter::render;
use crate::letter::work::TaskFront;
use crate::pump::flow::handlers::FlowMail;
use crate::pump::flow::tick::raise_once;
use crate::pump::flow::verify;
use crate::pump::host::Ctx;
use crate::pump::jobs::merge_main::ref_safe;
use crate::pump::jobs::{self, JobStatus, JobStore, rung};
use crate::pump::store::PumpStore;

/// Seconds that must pass between two main rung starts on one target.
const INTERVAL_SECS: u64 = 1800;

/// Exception kind raised when the target's main ref is absent on its remote.
pub(crate) const MAIN_REF_MISSING: &str = "flow-main-ref-missing";

/// Length of the tip prefix used in the job id.
const SHORT_TIP: usize = 12;

/// Enqueue a main rung job for `target` when it is due; returns the job id.
///
/// Returns `Ok(None)` when no main rung is configured, a main rung job is
/// still queued or running, the interval has not passed, main has no tip, or
/// the tip was already tested or failed on the environment. A main ref
/// absent on the remote raises [`MAIN_REF_MISSING`] once.
///
/// # Errors
/// A store, mail or git-config failure, or an unknown target.
pub(crate) fn schedule<S: JobStore, M: FlowMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &mut M,
    target: &str,
) -> Result<Option<String>, VivariumError> {
    if ctx
        .cfg
        .effective_rungs(Some(target))
        .command("main")
        .is_none()
        || in_flight(st, target)
    {
        return Ok(None);
    }
    let started = store
        .main_state(&key(STARTED, target))?
        .map(|text| text.parse::<u64>())
        .transpose()
        .map_err(|e| VivariumError::Other(format!("main rung start time: {e}")))?;
    if started.is_some_and(|at| ctx.now() < at + INTERVAL_SECS) {
        return Ok(None);
    }
    let tctx = ctx
        .for_target(target)
        .ok_or_else(|| VivariumError::Config(format!("unknown target {target:?}")))?;
    let Some(tip) = verify::remote_tip(&tctx, &tctx.cfg.remote, &tctx.cfg.main)
        .map_err(VivariumError::Other)?
    else {
        raise_once(
            ctx,
            MAIN_REF_MISSING,
            target,
            &format!(
                "main rung for target {target}: expected {} on {}; actual: absent",
                tctx.cfg.main, tctx.cfg.remote
            ),
        );
        return Ok(None);
    };
    for name in [TESTED, ENV_FAILED] {
        if store.main_state(&key(name, target))?.as_deref() == Some(tip.as_str()) {
            return Ok(None);
        }
    }
    let task = task_for(ctx, store, mail, target)?;
    let short = &tip[..tip.len().min(SHORT_TIP)];
    let step_task = format!("main-{}-{short}", ref_safe(target));
    let attempt = super::lost_count(store, target, &tip)? + 1;
    let mut spec = rung::spec(ctx, target, "main", &tip, &step_task, attempt)
        .ok_or_else(|| VivariumError::Config(format!("no main rung for target {target:?}")))?;
    spec.task = Some(task);
    if let Some(payload) = spec.payload.as_object_mut() {
        payload.insert(PAYLOAD_KEY.to_owned(), serde_json::json!(target));
    }
    let id = spec.id.clone();
    if st.jobs().contains_key(&id) {
        return Err(VivariumError::Other(format!(
            "main rung job {id} was not enqueued: the id already exists"
        )));
    }
    jobs::enqueue(st, spec)?;
    store.set_main_state(&key(STARTED, target), &ctx.now().to_string())?;
    Ok(Some(id))
}

/// Whether a main rung job for `target` is queued or running.
fn in_flight(st: &impl JobStore, target: &str) -> bool {
    st.jobs().values().any(|job| {
        job.kind == "rung"
            && job
                .payload
                .get(PAYLOAD_KEY)
                .and_then(|value| value.as_str())
                == Some(target)
            && matches!(job.status, JobStatus::Pending | JobStatus::Dispatched)
    })
}

/// The target's main rung task: the stored one, or a new one sent from and to
/// the pump identity and stored.
fn task_for<M: FlowMail>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    target: &str,
) -> Result<String, VivariumError> {
    if let Some(task) = store.main_state(&key(TASK, target))? {
        return Ok(task);
    }
    let pump = &ctx.cfg.reply_from;
    let body = render(
        &TaskFront::default(),
        &format!("pump main rung on {target}"),
    )
    .map_err(|e| VivariumError::Message(e.to_string()))?;
    let task = mail.send_step(
        pump,
        pump,
        &format!("pump: main rung on {target}"),
        &body,
        &[],
    )?;
    store.set_main_state(&key(TASK, target), &task)?;
    Ok(task)
}

#[cfg(test)]
#[path = "schedule_test.rs"]
mod tests;
