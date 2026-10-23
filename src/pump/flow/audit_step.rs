//! Handler for the auditor steps audit1 and audit2. N5 starts the audit job and reads its verdict here.

use serde_json::Value;

use crate::error::VivariumError;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler, role_job};
use crate::pump::flow::verify_inputs::{
    admitted_plan, branch_range, capped, latest_of_class, need_thread, requirements,
};
use crate::pump::jobs::JobStore;

/// The class of rows whose earlier audit a second pass carries.
const AUDITOR_CLASS: &str = "auditor";
/// The node of the rework step whose slices a second pass lists.
const REWORK_NODE: &str = "rework1";
/// Largest admitted plan the brief carries.
const PLAN_CAP: usize = 60_000;
/// Largest need thread the brief carries.
const THREAD_CAP: usize = 60_000;
/// Largest prior audit the brief carries.
const PRIOR_CAP: usize = 30_000;

/// Starts an audit1 or audit2 step as a read-only auditor job.
struct AuditStart;

impl StepHandler for AuditStart {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let need = &cx.run.need;
        let range = branch_range(cx.ctx, cx.store, need)?;
        let (Some(base), Some(tip)) = (range.base.clone(), range.tip.clone()) else {
            return Err(VivariumError::Message(format!(
                "need {need} has no base or tip to audit"
            )));
        };
        let brief = brief(cx, step, &range.branch, &range.target, &base, &tip)?;
        let mut spec = role_job(cx.ctx, cx.run, step, "flow-audit", "auditor", &brief);
        spec.read_only = true;
        spec.builtin_role = Some("auditor".into());
        spec.target = Some(range.target);
        spec.from_ref = Some(tip.clone());
        if base != tip {
            spec.with = vec![base];
        }
        Ok(StartAction::Job(Box::new(spec)))
    }
}

fn brief(
    cx: &StartCx<'_>,
    step: &StepRow,
    branch: &str,
    target: &str,
    base: &str,
    tip: &str,
) -> Result<String, VivariumError> {
    let need = &cx.run.need;
    let messages = need_thread(cx.ctx, need)?;
    let mut out = format!(
        "Pump verify mode\n\nneed: {need}\ntarget: {target}\nneed branch: {branch}\nreview range: {base}..{tip}\n"
    );
    out.push_str("\nThe need rung runs in parallel with this audit. There is no thread-freshness refusal: a change to the need thread restarts the run elsewhere.\n");
    out.push_str("\n## Admitted plan\n\n");
    match admitted_plan(cx.store, need)? {
        Some(plan) => out.push_str(&capped(&plan, PLAN_CAP)),
        None => out.push_str("(no admitted plan recorded)"),
    }
    out.push_str("\n\n## Need thread\n\n");
    out.push_str(&capped(&thread_lines(&messages), THREAD_CAP));
    out.push_str("\n\n## Requirements to account for\n\n");
    for requirement in requirements(cx.ctx, need, &messages) {
        out.push_str(&format!("- {}: {}\n", requirement.id, requirement.text));
    }
    out.push_str(&second_pass(cx, step)?);
    Ok(out)
}

/// The thread as `handle: body` lines.
fn thread_lines(messages: &[Value]) -> String {
    messages
        .iter()
        .filter_map(|message| {
            let handle = message["handle"].as_str()?;
            Some(format!(
                "{handle}: {}",
                message["body"].as_str().unwrap_or("").trim()
            ))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The prior audit and the rework slices, when this run audited before.
fn second_pass(cx: &StartCx<'_>, step: &StepRow) -> Result<String, VivariumError> {
    let rows = cx
        .store
        .step_payloads_of_run(&cx.run.need, cx.run.run)?
        .into_iter()
        .filter(|row| row.task != step.task)
        .collect::<Vec<_>>();
    let Some(prior) = latest_of_class(&rows, AUDITOR_CLASS) else {
        return Ok(String::new());
    };
    let front = serde_json::to_string_pretty(&prior.payload["front"])
        .map_err(|error| VivariumError::Message(format!("encode prior audit front: {error}")))?;
    let body = prior.payload["body"].as_str().unwrap_or("");
    let mut out = format!(
        "\n\n## Second pass: prior audit\n\n{}",
        capped(&format!("{front}\n\n{body}"), PRIOR_CAP)
    );
    out.push_str("\n\n## Rework slices\n\n");
    let slices = rows
        .iter()
        .rev()
        .find(|row| row.node == REWORK_NODE)
        .and_then(|row| row.payload["front"]["slice"].as_array());
    match slices {
        Some(slices) if !slices.is_empty() => {
            for slice in slices {
                out.push_str(&format!("- {slice}\n"));
            }
        }
        _ => out.push_str("(none recorded)\n"),
    }
    Ok(out)
}

pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(FlowKind::Build, NodeClass::Auditor, Box::new(AuditStart));
}

#[cfg(test)]
#[path = "audit_step_test.rs"]
mod tests;
