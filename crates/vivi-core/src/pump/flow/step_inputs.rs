//! Shared readers for role-step briefs: pool replies, the need thread, targets.
//!
//! The planner, admitter, slicer, fanout and reslice handlers build their
//! briefs from the same few inputs; they live here once.

use crate::pump::attempt_record::hand_message;
use crate::pump::flow::def::{FlowDef, NodeClass};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::outcome::role_job;
use crate::pump::flow::slice_dispatch::SliceMail;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::jobs::JobSpec;
use crate::pump::store::PumpStore;
use vivi_error::VivariumError;

/// A pool worker's result reply starts with this text.
pub const POOL_RESULT_MARKER: &str = "pool result ";

/// The longest need thread kept, in characters.
const THREAD_CAP: usize = 40_000;

/// The agent's final message of every pool result reply on `task`, newest first.
///
/// # Errors
/// Returns [`VivariumError`] when the task thread cannot be read.
pub fn step_replies(mail: &dyn SliceMail, task: &str) -> Result<Vec<String>, VivariumError> {
    let mut replies = mail.marked_replies(task, POOL_RESULT_MARKER)?;
    replies.reverse();
    Ok(replies
        .into_iter()
        .map(|(_, text)| hand_message(&text))
        .collect())
}

/// The thread of `need` (`need show`), cut at 40 000 characters.
///
/// # Errors
/// Returns [`VivariumError`] when `need show` fails.
pub fn need_thread(ctx: &Ctx, need: &str) -> Result<String, VivariumError> {
    let out = ctx.vivi(&format!("need show {}", shell_quote(need)), 60);
    if out.code != 0 {
        return Err(VivariumError::Message(format!(
            "need show {need} failed (exit {})",
            out.code
        )));
    }
    match out.text.char_indices().nth(THREAD_CAP) {
        Some((at, _)) => Ok(format!("{}\n[thread cut]", &out.text[..at])),
        None => Ok(out.text),
    }
}

/// The ids of every configured target.
#[must_use]
pub fn targets(ctx: &Ctx) -> Vec<String> {
    ctx.cfg.target_configs().into_keys().collect()
}

/// The last Done row of `node` in the run.
///
/// # Errors
/// Returns [`VivariumError`] when the step rows cannot be read.
pub fn done_step(
    store: &PumpStore,
    run: &RunRow,
    node: &str,
) -> Result<Option<StepRow>, VivariumError> {
    Ok(store
        .steps_of(&run.need, run.run)?
        .into_iter()
        .rfind(|row| row.node == node && row.state == StepState::Done))
}

/// The newest Done row of a node of `class` that reaches `node` through one
/// decision node (`P -> D -> node`).
///
/// # Errors
/// Returns [`VivariumError`] when the step rows cannot be read.
pub fn predecessor_step(
    def: &FlowDef,
    store: &PumpStore,
    run: &RunRow,
    node: &str,
    class: &NodeClass,
) -> Result<Option<StepRow>, VivariumError> {
    let is_decision = |id: &str| {
        def.nodes
            .iter()
            .any(|n| n.id == id && n.class == NodeClass::Decision)
    };
    let is_class = |id: &str| def.nodes.iter().any(|n| n.id == id && &n.class == class);
    let feeds = |id: &str| {
        def.edges.iter().any(|d| {
            is_decision(&d.from)
                && d.to == node
                && def.edges.iter().any(|p| p.to == d.from && p.from == id)
        })
    };
    Ok(store
        .steps_of(&run.need, run.run)?
        .into_iter()
        .rfind(|row| row.state == StepState::Done && is_class(&row.node) && feeds(&row.node)))
}

/// A read-only job for a role step: all targets beside the checkout, the main
/// ref as the start point. The budget comes from the node's class.
///
/// # Errors
/// Returns [`VivariumError::Config`] when `role` is missing or not active, or
/// when `class` is not a role class.
pub fn read_only_job(
    ctx: &Ctx,
    run: &RunRow,
    step: &StepRow,
    class: &NodeClass,
    role: &str,
    brief: &str,
) -> Result<JobSpec, VivariumError> {
    let found = ctx
        .routing
        .lookup(role)
        .filter(|found| found.status == "active")
        .ok_or_else(|| VivariumError::Config(format!("role {role} is missing or not active")))?;
    let execution = class.role_kind().ok_or_else(|| {
        VivariumError::Config(format!(
            "node class {} has no role kind to budget",
            class.name()
        ))
    })?;
    let mut spec = role_job(ctx, run, step, &class.name(), execution, &found.name, brief);
    make_read_only(ctx, &mut spec);
    Ok(spec)
}

/// Makes `spec` a read-only job: all targets beside the checkout, and the
/// main ref of the target its own checkout comes from as the start point.
pub fn make_read_only(ctx: &Ctx, spec: &mut JobSpec) {
    spec.read_only = true;
    spec.from_ref = Some(read_only_start(&ctx.cfg, spec.target.as_deref()));
    spec.extra_targets = targets(ctx);
}

/// The start ref of a read-only job's own checkout: the main of its target
/// (the job's own, else the primary target the dispatcher gives a job with
/// none), the same rule its extra checkouts follow. The workspace-level
/// `[pump] main` is only the legacy single-repository fallback: with targets
/// it names the upstream mirror, not the integration branch the work is on.
fn read_only_start(cfg: &crate::pump::config::PumpConfig, target: Option<&str>) -> String {
    let target = target.map(str::to_owned).or_else(|| cfg.primary_target());
    match target.and_then(|id| cfg.for_target(&id)) {
        Some(target_cfg) => target_cfg.dispatch_from().to_owned(),
        None => cfg.dispatch_from().to_owned(),
    }
}

#[cfg(test)]
#[path = "step_inputs_test.rs"]
mod tests;
