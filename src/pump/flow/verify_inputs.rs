//! Inputs for the audit, judge, rework and CTO briefs. N5 gathers the facts
//! each brief needs here; this module registers nothing.
//!
//! Pool workers have no mailspace, so earlier replies come from the step
//! payload table, and the need thread comes from `vivi need show`.

use serde_json::Value;

use crate::error::VivariumError;
use crate::pump::flow::need_branch;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::need_audit::{NeedAuditRequirement, whole_message_requirements};
use crate::pump::store::PumpStore;
use crate::pump::store::step_payload::StepPayloadRow;

/// The planner verdict that marks an admitted plan.
const PLANNED: &str = "planned";

/// The need branch of a need and the range an audit reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchRange {
    pub branch: String,
    pub base: Option<String>,
    pub tip: Option<String>,
    pub target: String,
}

/// The full thread of a need as `vivi need show` JSON messages.
///
/// # Errors
/// Returns [`VivariumError`] when the command fails, prints no valid array,
/// or the array holds no root message of the need.
pub(crate) fn need_thread(ctx: &Ctx, need: &str) -> Result<Vec<Value>, VivariumError> {
    let command = format!(
        "need show {} --newest {} --json",
        shell_quote(need),
        usize::MAX
    );
    let output = ctx.vivi(&command, 60);
    if output.code != 0 {
        return Err(VivariumError::Message(format!(
            "cannot read full need thread {need}: {}",
            crate::pump::collect::tail(&output.text, 300)
        )));
    }
    let start = output.text.find('[').ok_or_else(|| {
        VivariumError::Message(format!("need show returned no thread array for {need}"))
    })?;
    let messages = serde_json::from_str::<Vec<Value>>(&output.text[start..]).map_err(|error| {
        VivariumError::Message(format!(
            "need show returned invalid thread JSON for {need}: {error}"
        ))
    })?;
    if !messages
        .iter()
        .any(|message| message["handle"].as_str() == Some(need))
    {
        return Err(VivariumError::Message(format!(
            "need show returned no root message for {need}"
        )));
    }
    Ok(messages)
}

/// The requirements an audit must account for, one per whole need message.
pub(crate) fn requirements(ctx: &Ctx, need: &str, messages: &[Value]) -> Vec<NeedAuditRequirement> {
    whole_message_requirements(ctx, need, messages)
}

/// The newest admitted plan of a need, over every run. Runs restarted at
/// slice have no planner step, which is why this searches the whole need.
///
/// # Errors
/// Returns [`VivariumError`] when the store cannot be read.
pub(crate) fn admitted_plan(
    store: &PumpStore,
    need: &str,
) -> Result<Option<String>, VivariumError> {
    let rows = store.step_payloads_of_need(need)?;
    for row in rows.iter().rev().filter(|row| row.class == "planner") {
        let planned = store
            .step_by_task(&row.task)?
            .is_some_and(|step| step.verdict.as_deref() == Some(PLANNED));
        if planned {
            return Ok(row.payload["body"].as_str().map(str::to_string));
        }
    }
    Ok(None)
}

/// The need branch row and its current tip.
///
/// # Errors
/// Returns [`VivariumError`] naming the need when there is no need branch
/// row, or when the store or the remote cannot be read.
pub(crate) fn branch_range(
    ctx: &Ctx,
    store: &PumpStore,
    need: &str,
) -> Result<BranchRange, VivariumError> {
    let row = store
        .need_branch(need)?
        .ok_or_else(|| VivariumError::Message(format!("need {need} has no need branch row")))?;
    let tip = need_branch::tip(ctx, store, need)?;
    Ok(BranchRange {
        branch: row.branch,
        base: row.base,
        tip,
        target: row.target,
    })
}

/// The newest row of a class.
pub(crate) fn latest_of_class<'a>(
    rows: &'a [StepPayloadRow],
    class: &str,
) -> Option<&'a StepPayloadRow> {
    rows.iter().rev().find(|row| row.class == class)
}

/// A rung payload as plain text: the exit code, the sorted failing ids and
/// the output tail.
pub(crate) fn rung_report(row: &StepPayloadRow) -> String {
    let payload = &row.payload;
    let exit = match payload["exit"].as_i64() {
        Some(code) => code.to_string(),
        None => String::from("none"),
    };
    let mut failing: Vec<&str> = payload["failing"]
        .as_array()
        .map(|ids| ids.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    failing.sort_unstable();
    let failing = if failing.is_empty() {
        String::from("none")
    } else {
        failing.join(", ")
    };
    let tail = payload["tail"].as_str().unwrap_or("");
    format!("exit: {exit}\nfailing: {failing}\ntail:\n{tail}")
}

/// The last `max` bytes of `text` on a char boundary, with a one-line marker
/// when the start was cut.
pub(crate) fn capped(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("[...]\n{}", &text[start..])
}

#[cfg(test)]
#[path = "verify_inputs_test.rs"]
mod tests;
