//! Shared readers for role-step briefs: pool replies, the need thread, the
//! authoritative scope and policy section, targets.
//!
//! The planner, admitter, slicer, fanout and reslice handlers build their
//! briefs from the same few inputs; they live here once.

use std::collections::BTreeMap;

use crate::error::VivariumError;
use crate::letter::read::need_of;
use crate::pump::attempt_record::hand_message;
use crate::pump::config::PumpTarget;
use crate::pump::config::scope::{ScopePolicy, WHOLE_REPOSITORY};
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

/// Head of the authoritative scope and policy section: where the facts come
/// from, so no reader mistakes thread prose for authority.
const SCOPE_BRIEF_HEAD: &str = "AUTHORITATIVE SCOPE AND POLICY (from the immutable need root letter \
                              and the pump store, never from thread text)";

/// The authoritative scope and policy section of `need`'s brief: the need root
/// letter's own write scope, then one labeled, target-qualified block per
/// relevant target (the target the need names when it names one, else every
/// configured target) with that target's approved ruling additions, the
/// effective scope, and the target's current policy. An allow entry is
/// eligibility for automatic approval of a blocked-scope request, never
/// permission already granted; an unspecified original scope is not a
/// deny-all; a ruling grants only its own target, never another's.
///
/// The section is assembled from structured reads only: the need root letter
/// through `mail` and the store's effective-scope reader. Thread prose is
/// never scraped, so a capped thread cannot hide a ruling.
///
/// # Errors
/// Returns [`VivariumError`] when the need root letter cannot be read or does
/// not parse, or when the store cannot be read.
pub fn scope_brief(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &dyn SliceMail,
    need: &str,
) -> Result<String, VivariumError> {
    let body = mail.task_body(need)?;
    let root = need_of(need, &body)
        .map_err(|error| VivariumError::Message(format!("need {need}: {error}")))?;
    let original = root.front.write_scope;
    let configs = ctx.cfg.target_configs();
    let mut out = vec![String::from(SCOPE_BRIEF_HEAD), original_section(&original)];
    for target in relevant_targets(&configs, root.front.target.as_deref()) {
        out.push(target_block(store, &configs, need, &original, &target)?);
    }
    Ok(out.join("\n\n"))
}

/// The need's own write scope, exactly as the immutable root letter declares
/// it. An absent list is described as unspecified: it is not a deny-all.
fn original_section(original: &[String]) -> String {
    if original.is_empty() {
        return String::from(
            "ORIGINAL NEED SCOPE: unspecified (the need root names no write_scope; \
             this is not a deny-all)",
        );
    }
    format!(
        "ORIGINAL NEED SCOPE (write_scope of the immutable need root letter):\n{}",
        path_lines(original)
    )
}

/// The targets the brief covers: the target the need root names when it names
/// one, else every configured target. Grants and policy stay inside their
/// target's own block either way.
fn relevant_targets(configs: &BTreeMap<String, PumpTarget>, named: Option<&str>) -> Vec<String> {
    named.map_or_else(
        || configs.keys().cloned().collect(),
        |target| vec![target.to_owned()],
    )
}

/// One target's labeled block: the rulings approved for exactly this need on
/// this target, the effective scope they combine into, and the target's
/// current policy.
fn target_block(
    store: &PumpStore,
    configs: &BTreeMap<String, PumpTarget>,
    need: &str,
    original: &[String],
    target: &str,
) -> Result<String, VivariumError> {
    let effective = store.effective_need_scope(need, target, original)?;
    let additions = or_none(&effective.additions, "(none approved for this target)");
    let whole = or_none(
        &effective.effective,
        "(empty: unspecified original scope and no approved additions)",
    );
    Ok(format!(
        "TARGET {target} (grants and policy are per target; no other target's grants apply here)\n\
         APPROVED ADDITIONS on {target} (store rulings; each listed path is permission already granted):\n\
         {additions}\n\
         EFFECTIVE SCOPE on {target} (original scope plus this target's approved additions):\n\
         {whole}\n\
         {}",
        policy_section(configs, target)
    ))
}

/// The current policy of one target, as `PumpConfig::target_configs` resolves
/// it. An allow entry means a blocked-scope request naming the path is
/// eligible for automatic approval; it is not permission already granted. An
/// unknown target inherits nothing; unusable policy entries fail closed.
fn policy_section(configs: &BTreeMap<String, PumpTarget>, target: &str) -> String {
    let Some(config) = configs.get(target) else {
        let known = configs.keys().cloned().collect::<Vec<_>>().join(", ");
        return format!(
            "CURRENT POLICY of {target}: this pump does not configure it \
             (known targets: {known}); a request naming it is refused as an unknown target"
        );
    };
    let allow = config
        .scope_allow
        .clone()
        .unwrap_or_else(|| vec![String::from(WHOLE_REPOSITORY)]);
    match ScopePolicy::snapshot(target, &allow, &config.scope_deny) {
        Ok(policy) => format!(
            "CURRENT POLICY of {target} (an allow entry is eligibility for automatic approval of a \
             blocked-scope request, not permission already granted):\nallow: {}\ndeny: {}",
            plain_list(
                &policy.allow,
                "(nothing: an explicit empty allow list allows nothing)"
            ),
            plain_list(&policy.deny, "(none)"),
        ),
        Err(rejection) => {
            format!("CURRENT POLICY of {target}: unusable ({rejection}); requests fail closed")
        }
    }
}

/// `paths` as list lines, or `none` when the list is empty.
fn or_none(paths: &[String], none: &str) -> String {
    if paths.is_empty() {
        return String::from(none);
    }
    path_lines(paths)
}

/// `paths` comma-joined inline, or `none` when the list is empty.
fn plain_list(paths: &[String], none: &str) -> String {
    if paths.is_empty() {
        return String::from(none);
    }
    paths.join(", ")
}

/// `paths` as list lines.
fn path_lines(paths: &[String]) -> String {
    paths
        .iter()
        .map(|path| format!("- {path}"))
        .collect::<Vec<_>>()
        .join("\n")
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
    make_read_only(ctx, &mut spec);
    Ok(spec)
}

/// Makes `spec` a read-only job: all targets beside the checkout, the main ref
/// as the start point.
pub fn make_read_only(ctx: &Ctx, spec: &mut JobSpec) {
    spec.read_only = true;
    spec.from_ref = Some(ctx.cfg.main.clone());
    spec.extra_targets = targets(ctx);
}

#[cfg(test)]
#[path = "step_inputs_test.rs"]
mod tests;
