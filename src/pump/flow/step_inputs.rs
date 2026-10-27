//! Shared readers for role-step briefs: pool replies, the need thread, targets.
//!
//! The planner, admitter, slicer, fanout and reslice handlers build their
//! briefs from the same few inputs; they live here once.

use crate::error::VivariumError;
use crate::pump::attempt_record::hand_message;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::flow::def::{FlowDef, NodeClass};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::outcome::role_job;
use crate::pump::flow::slice_dispatch::SliceMail;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::jobs::JobSpec;
use crate::pump::store::PumpStore;

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

/// The typed `target:` of the original need letter under `need`, or `None`
/// when a valid need letter carries no target.
///
/// # Errors
/// Returns [`VivariumError`] when the body of `need` cannot be read, and
/// [`VivariumError::Message`] naming `need` when its body is not a valid need
/// letter: a legacy body without front matter, malformed TOML and invalid
/// typed front matter stay errors and never fall back to another selection.
pub fn need_target(mail: &dyn SliceMail, need: &str) -> Result<Option<String>, VivariumError> {
    let body = mail.task_body(need)?;
    let envelope = crate::letter::read::need_of(need, &body)
        .map_err(|error| VivariumError::Message(format!("need {need}: {error}")))?;
    Ok(envelope.front.target)
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
    let execution = RoleKind::from_class(class).ok_or_else(|| {
        VivariumError::Config(format!(
            "node class {} has no role kind to budget",
            class.name()
        ))
    })?;
    let mut spec = role_job(ctx, run, step, &class.name(), execution, &found.name, brief);
    make_read_only(ctx, &mut spec)?;
    Ok(spec)
}

/// Makes `spec` a read-only job: all targets beside the checkout, started at
/// the job target's main — the explicit `spec.target` wins, else the
/// configuration's primary target, resolved through
/// [`crate::pump::config::PumpConfig::for_target`].
///
/// # Errors
/// Returns [`VivariumError::Config`] when `spec` names an unknown target, or
/// names none while the configuration has several explicit targets and no
/// `default`. Either failure leaves `spec` unchanged.
pub fn make_read_only(ctx: &Ctx, spec: &mut JobSpec) -> Result<(), VivariumError> {
    spec.start_from_target_main(&ctx.cfg)?;
    spec.read_only = true;
    spec.extra_targets = targets(ctx);
    Ok(())
}

#[cfg(test)]
#[path = "step_inputs_test.rs"]
mod tests;
