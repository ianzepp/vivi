//! Node override handler for the build node rework1: the slicer in rework
//! mode. It reads the audit findings, requirement gaps, judge reason and
//! failing rung output from the stored step payloads and cuts rework slices.

use serde_json::Value;

use crate::error::VivariumError;
use crate::mailspace::shipped::SLICER_ROLE;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fanout::{VERDICT_MERGED, VERDICT_NO_DEFECT, is_slice_row};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{self, StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs::make_read_only;
use crate::pump::flow::verify_inputs::{
    admitted_plan, audit_context, branch_range, capped, cto_ruling, latest_of_class, rung_report,
};
use crate::pump::jobs::JobStore;
use crate::pump::store::step_payload::StepPayloadRow;

/// The build node this handler overrides.
const REWORK_NODE: &str = "rework1";
/// The pool job kind of a rework slicer.
const JOB_KIND: &str = "flow-rework";
/// Most bytes of the admitted plan in the brief.
const PLAN_CAP: usize = 60_000;
/// Requirement statuses that are not gaps.
const NOT_GAPS: [&str; 2] = ["SATISFIED", "NOT_APPLICABLE"];

/// Starts the rework slicer of a build run.
struct ReworkStart;

impl StepHandler for ReworkStart {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let brief = brief(cx)?;
        let mut spec = outcome::role_job(
            cx.ctx,
            cx.run,
            step,
            JOB_KIND,
            RoleKind::Slicer,
            SLICER_ROLE,
            &brief,
        );
        make_read_only(cx.ctx, &mut spec);
        Ok(StartAction::Job(Box::new(spec)))
    }
}

fn brief(cx: &StartCx<'_>) -> Result<String, VivariumError> {
    let need = &cx.run.need;
    let range = branch_range(cx.ctx, cx.store, need)?;
    let plan = admitted_plan(cx.store, need)?;
    let (context_run, rows) = audit_context(cx.store, need, cx.run.run)?;
    let mut text = format!(
        "mode: rework\nneed: {need}\ntarget: {}\nneed branch: {}\n\n",
        range.target, range.branch
    );
    if context_run != cx.run.run {
        text.push_str(&format!(
            "context: run {context_run} (this run restarted at rework1)\n\n"
        ));
    }
    text.push_str(
        "Cut rework slices that go on the existing need branch and fix exactly the findings, \
         requirement gaps and failing rung output below. Do not re-cut merged slices.\n\n",
    );
    text.push_str("## Admitted plan\n\n");
    match plan {
        Some(plan) => text.push_str(&capped(&plan, PLAN_CAP)),
        None => text.push_str("(no admitted plan recorded)"),
    }
    text.push_str("\n\n## Slices already merged on the need branch\n\n");
    text.push_str(&merged_slices(cx, need, context_run)?);
    text.push_str("\n\n## Audit findings and requirement gaps\n\n");
    text.push_str(&audit_section(&rows));
    if let Some(reason) = latest_of_class(&rows, "judge").and_then(|row| front_str(row, "reason")) {
        text.push_str(&format!("\n\n## Judge reason\n\n{reason}"));
    }
    text.push_str("\n\n## Failing rung output\n\n");
    text.push_str(&rung_section(&rows));
    if context_run != cx.run.run {
        text.push_str("\n\n## CTO ruling\n\n");
        match cto_ruling(cx.store, need, cx.run.run)? {
            Some(ruling) => text.push_str(&ruling),
            None => text.push_str("(no CTO ruling; an operator unlock restarted the run)"),
        }
    }
    Ok(text)
}

fn merged_slices(cx: &StartCx<'_>, need: &str, context_run: u32) -> Result<String, VivariumError> {
    let mut steps = cx.store.steps_of(need, cx.run.run)?;
    if context_run != cx.run.run {
        steps.extend(cx.store.steps_of(need, context_run)?);
    }
    let lines: Vec<String> = steps
        .iter()
        .filter(|row| is_slice_row(row))
        .filter(|row| {
            matches!(
                row.verdict.as_deref(),
                Some(VERDICT_MERGED | VERDICT_NO_DEFECT)
            )
        })
        .map(|row| format!("- {} (task {})", row.node, row.task))
        .collect();
    Ok(none_if_empty(lines))
}

fn audit_section(rows: &[StepPayloadRow]) -> String {
    let Some(audit) = latest_of_class(rows, "auditor") else {
        return String::from("(no audit recorded)");
    };
    let mut lines = Vec::new();
    for finding in array_of(&audit.payload["front"]["finding"]) {
        lines.push(format!(
            "- finding {}: {}\n  paths: {}\n  recheck: {}",
            text_of(&finding["id"]),
            text_of(&finding["content"]),
            join_strs(&finding["paths"]),
            text_of(&finding["recheck_trigger"]),
        ));
    }
    for gap in array_of(&audit.payload["front"]["requirement"]) {
        let status = text_of(&gap["status"]);
        if NOT_GAPS.contains(&status.as_str()) {
            continue;
        }
        lines.push(format!(
            "- requirement {} [{status}]: {}\n  evidence: {}",
            text_of(&gap["id"]),
            text_of(&gap["text"]),
            join_strs(&gap["evidence"]),
        ));
    }
    none_if_empty(lines)
}

fn rung_section(rows: &[StepPayloadRow]) -> String {
    let reports: Vec<String> = rows
        .iter()
        .filter(|row| row.class.starts_with("rung-"))
        .filter(|row| row.payload["exit"].as_i64() != Some(0))
        .map(|row| format!("### {} ({})\n{}", row.node, row.task, rung_report(row)))
        .collect();
    if reports.is_empty() {
        String::from("(none)")
    } else {
        reports.join("\n\n")
    }
}

fn none_if_empty(lines: Vec<String>) -> String {
    if lines.is_empty() {
        String::from("(none)")
    } else {
        lines.join("\n")
    }
}

fn front_str(row: &StepPayloadRow, key: &str) -> Option<String> {
    row.payload["front"][key]
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn array_of(value: &Value) -> Vec<&Value> {
    value
        .as_array()
        .map(|items| items.iter().collect())
        .unwrap_or_default()
}

fn text_of(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn join_strs(value: &Value) -> String {
    let items: Vec<&str> = array_of(value)
        .into_iter()
        .filter_map(Value::as_str)
        .collect();
    if items.is_empty() {
        String::from("none")
    } else {
        items.join(", ")
    }
}

pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.node_handler(FlowKind::Build, REWORK_NODE, Box::new(ReworkStart));
}

#[cfg(test)]
#[path = "rework_step_test.rs"]
mod tests;
