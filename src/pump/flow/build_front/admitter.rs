//! The admitter step handler (admit1, admit2): a read-only admitter role job.

use crate::error::VivariumError;
use crate::mailspace::shipped::ADMITTER_ROLE;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::need_scope::{TargetScope, effective_scopes};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs;
use crate::pump::jobs::JobStore;

/// Starts an admitter step: the plan to check and the need thread.
struct AdmitterStep;

impl StepHandler for AdmitterStep {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let plan = plan_of(cx, step)?;
        let brief = format!(
            "need: {}\nrun: {}\n\n{}{}\nplan:\n{plan}\n\nthread:\n{}",
            cx.run.need,
            cx.run.run,
            scope_block(cx, &cx.run.need)?,
            contract_block(),
            step_inputs::need_thread(cx.ctx, &cx.run.need)?,
        );
        let spec = step_inputs::read_only_job(
            cx.ctx,
            cx.run,
            step,
            &NodeClass::Admitter,
            ADMITTER_ROLE,
            &brief,
        )?;
        Ok(StartAction::Job(Box::new(spec)))
    }
}

/// The authoritative effective need write scope: the original letter's
/// delegation plus every later pump grant, grouped by target, kept outside
/// the truncated need thread. A target left out holds no delegated paths,
/// and an empty scope is not unrestricted permission.
fn scope_block(cx: &StartCx<'_>, need: &str) -> Result<String, VivariumError> {
    let groups = effective_scopes(
        cx.mail,
        need,
        &cx.ctx.cfg.reply_from,
        &step_inputs::targets(cx.ctx),
    )?;
    let mut text = String::from(
        "effective need write scope (grouped by target; the original letter's delegation plus every later pump grant; authoritative for this job and supersedes the original letter's write_scope):\n",
    );
    if groups.is_empty() {
        text.push_str(
            "- (none): the need delegates no write scope; that is not unrestricted permission\n",
        );
        return Ok(text);
    }
    for group in &groups {
        text.push_str(&target_line(group));
    }
    text.push_str("a target absent above holds no delegated paths\n");
    Ok(text)
}

/// One grouped target line: delegated paths first, then later grants.
fn target_line(group: &TargetScope) -> String {
    let mut line = format!("- target {}: ", group.target);
    if !group.original.is_empty() {
        line.push_str(&format!("delegated {}", group.original.join(", ")));
    }
    if !group.granted.is_empty() {
        if !group.original.is_empty() {
            line.push_str("; ");
        }
        line.push_str(&format!("granted {}", group.granted.join(", ")));
    }
    line.push('\n');
    line
}

/// The runtime scope-request contract: how this role asks for permission,
/// independent of any installed charter.
fn contract_block() -> &'static str {
    "scope request contract (independent of installed charters):\n\
     - a permission-only request answers `escalate` with nonblank `scope_target`, nonempty `scope_paths`, and a `reason`\n\
     - a general operator or design question omits the scope fields\n\
     - target policy may grant additions automatically; it never grants concurrent slice ownership\n"
}

/// The newest reply of the planner step that feeds this admitter step.
fn plan_of(cx: &StartCx<'_>, step: &StepRow) -> Result<String, VivariumError> {
    let node = &step.node;
    let planner =
        step_inputs::predecessor_step(cx.def, cx.store, cx.run, node, &NodeClass::Planner)?
            .ok_or_else(|| {
                VivariumError::Parse(format!("admitter step {node} has no finished planner step"))
            })?;
    step_inputs::step_replies(cx.mail, &planner.task)?
        .into_iter()
        .next()
        .ok_or_else(|| {
            VivariumError::Parse(format!(
                "admitter step {node}: planner step {} has no reply",
                planner.node
            ))
        })
}

/// Register the admitter handler for the build flow.
pub(super) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(FlowKind::Build, NodeClass::Admitter, Box::new(AdmitterStep));
}

#[cfg(test)]
#[path = "admitter_test.rs"]
mod tests;
