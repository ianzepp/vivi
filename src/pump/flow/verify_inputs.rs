//! Inputs for the audit, judge, rework and CTO briefs. N5 gathers the facts
//! each brief needs here; this module registers nothing.
//!
//! Pool workers have no mailspace, so earlier replies come from the step
//! payload table, and the need thread comes from `vivi need show`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::VivariumError;
use crate::pump::flow::need_branch;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::store::PumpStore;
use crate::pump::store::step_payload::StepPayloadRow;

/// The planner verdict that marks an admitted plan.
const PLANNED: &str = "planned";

/// The class of an audit step.
const AUDITOR: &str = "auditor";

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

/// One original requirement passed to the Auditor for status accounting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeedAuditRequirement {
    pub id: String,
    pub text: String,
}

/// The requirements an audit must account for. A root message with front
/// matter gives one requirement per `done_when` entry, ids `<handle>-d<n>`
/// (1-based); every later message (an amendment) is one whole-message
/// requirement.
///
/// # Errors
/// Returns [`VivariumError`] naming the handle when the root is not a valid
/// need letter.
pub(crate) fn requirements(
    ctx: &Ctx,
    need: &str,
    messages: &[Value],
) -> Result<Vec<NeedAuditRequirement>, VivariumError> {
    let whole = whole_message_requirements(ctx, need, messages);
    let Some(root) = messages.first() else {
        return Ok(whole);
    };
    let handle = root["handle"].as_str().unwrap_or("");
    let body = root["body"].as_str().unwrap_or("");
    let envelope = crate::letter::read::need_of(handle, body)
        .map_err(|error| VivariumError::Message(format!("need {handle}: {error}")))?;
    let root_id = format!("{handle}-r0");
    let mut out: Vec<NeedAuditRequirement> = envelope
        .front
        .done_when
        .into_iter()
        .enumerate()
        .map(|(index, text)| NeedAuditRequirement {
            id: format!("{handle}-d{}", index + 1),
            text,
        })
        .collect();
    out.extend(whole.into_iter().filter(|req| req.id != root_id));
    Ok(out)
}

/// One requirement per source message: the requirement text is one short line
/// that names the message and points at the thread the auditor already
/// receives as `original_need_thread`: a receipt must repeat each requirement
/// text byte for byte, and a model auditor cannot reliably echo a long body.
/// Generated pump replies are skipped.
pub fn whole_message_requirements(
    ctx: &Ctx,
    need: &str,
    messages: &[Value],
) -> Vec<NeedAuditRequirement> {
    messages
        .iter()
        .enumerate()
        .filter(|(index, message)| {
            !crate::pump::amend::is_generated_pump_reply(ctx, need, *index, message)
        })
        .filter_map(|(_, message)| {
            let handle = message["handle"].as_str().filter(|h| !h.is_empty())?;
            let body = message["body"].as_str().unwrap_or("").trim();
            if body.is_empty() {
                return None;
            }
            Some(NeedAuditRequirement {
                id: format!("{handle}-r0"),
                text: format!(
                    "Deliver what need message {handle} asks for (full text in the original need thread)."
                ),
            })
        })
        .collect()
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

/// The step payload rows a brief reads for audit context, with the run they
/// came from. A run restarted at rework1 has no audit of its own, so the rows
/// come from the newest earlier run that holds an auditor row. When no run
/// has an audit, the asked run's rows come back.
///
/// # Errors
/// Returns [`VivariumError`] when the store cannot be read.
pub(crate) fn audit_context(
    store: &PumpStore,
    need: &str,
    run: u32,
) -> Result<(u32, Vec<StepPayloadRow>), VivariumError> {
    let own = store.step_payloads_of_run(need, run)?;
    if own.iter().any(|row| row.class == AUDITOR) {
        return Ok((run, own));
    }
    let mut earlier: Vec<(u32, Vec<StepPayloadRow>)> = Vec::new();
    for row in store.step_payloads_of_need(need)? {
        if row.run >= run {
            continue;
        }
        match earlier.last_mut() {
            Some((number, rows)) if *number == row.run => rows.push(row),
            _ => earlier.push((row.run, vec![row])),
        }
    }
    let found = earlier
        .into_iter()
        .rev()
        .find(|(_, rows)| rows.iter().any(|row| row.class == AUDITOR));
    Ok(found.unwrap_or((run, own)))
}

/// The newest CTO answer from a run below `run`, as `answer`, `reason` and
/// `new_fact` lines. None when no earlier run has a CTO row.
///
/// # Errors
/// Returns [`VivariumError`] when the store cannot be read.
pub(crate) fn cto_ruling(
    store: &PumpStore,
    need: &str,
    run: u32,
) -> Result<Option<String>, VivariumError> {
    let rows = store.step_payloads_of_need(need)?;
    let Some(row) = rows
        .iter()
        .rev()
        .find(|row| row.run < run && row.class == "cto")
    else {
        return Ok(None);
    };
    let lines: Vec<String> = ["answer", "reason", "new_fact"]
        .iter()
        .filter_map(|key| {
            row.payload["front"][key]
                .as_str()
                .map(|value| format!("{key}: {value}"))
        })
        .collect();
    Ok(Some(lines.join("\n")))
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
