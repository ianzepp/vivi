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

/// The `target:` of `need`'s root letter, or `None` when it names none.
///
/// # Errors
/// Returns [`VivariumError`] when the root body cannot be read or is not a
/// valid need letter; `need` is named in any parsing error.
pub fn need_target(mail: &dyn SliceMail, need: &str) -> Result<Option<String>, VivariumError> {
    let body = mail.task_body(need)?;
    let envelope = crate::letter::read::need_of(need, &body)
        .map_err(|error| VivariumError::Message(format!("need {need}: {error}")))?;
    Ok(envelope.front.target)
}

/// A read-only job for a role step: all targets beside the checkout, the main
/// ref of `target` as the start point. The budget comes from the node's class.
/// `target` is the need's own target; `None` resolves the primary (or named
/// `default`) target of the configuration.
///
/// # Errors
/// Returns [`VivariumError::Config`] when `role` is missing or not active,
/// when `class` is not a role class, or when `target` cannot be resolved.
pub fn read_only_job(
    ctx: &Ctx,
    run: &RunRow,
    step: &StepRow,
    class: &NodeClass,
    role: &str,
    brief: &str,
    target: Option<String>,
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
    spec.target = target;
    make_read_only(ctx, &mut spec)?;
    Ok(spec)
}

/// Makes `spec` a read-only job: all targets beside the checkout, the
/// resolved main ref as the start point.
///
/// The target resolves before `spec` changes: an explicit `spec.target` wins;
/// otherwise the primary target of the configuration is selected, falling
/// back to the synthetic legacy `default` when no targets are named. A legacy
/// repository selection keeps `spec.target` unset; a named selection is
/// persisted on `spec.target`, and an explicit one is preserved.
///
/// # Errors
/// Returns [`VivariumError::Config`] when the explicit target is unknown, or
/// when several targets are named without a `default`.
pub fn make_read_only(ctx: &Ctx, spec: &mut JobSpec) -> Result<(), VivariumError> {
    let (target, main) = resolved_target(ctx, spec)?;
    spec.read_only = true;
    spec.from_ref = Some(main);
    spec.extra_targets = targets(ctx);
    spec.target = target;
    Ok(())
}

/// The effective target of a read-only start and the main ref of its
/// configuration copy. Every error is decided before any specification
/// mutation, so a refused start leaves `spec` untouched.
fn resolved_target(ctx: &Ctx, spec: &JobSpec) -> Result<(Option<String>, String), VivariumError> {
    let id = match spec.target.clone() {
        Some(explicit) => explicit,
        None if ctx.cfg.targets.is_empty() => {
            // Legacy single-repository settings: the synthetic `default`
            // target carries the pool's own repository selection, and the
            // spec stays target-less so the pool checkout is used.
            let cfg = ctx.cfg.for_target("default").ok_or_else(|| {
                VivariumError::Config(String::from("legacy config has no default target"))
            })?;
            return Ok((None, cfg.main));
        }
        None => ctx.cfg.primary_target().ok_or_else(|| {
            VivariumError::Config(String::from(
                "several targets are configured and none is named `default`; set the target",
            ))
        })?,
    };
    let cfg = ctx
        .cfg
        .for_target(&id)
        .ok_or_else(|| VivariumError::Config(format!("target {id} is not configured")))?;
    Ok((Some(id), cfg.main))
}

#[cfg(test)]
#[path = "step_inputs_test.rs"]
mod tests;
