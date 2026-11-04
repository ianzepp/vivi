//! Node override handler for the build node rework1: the slicer in rework
//! mode. It reads the audit findings, requirement gaps, judge reason and
//! failing rung output from the stored step payloads and cuts rework slices.

use serde_json::Value;

use crate::mailspace::shipped::SLICER_ROLE;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fanout::{VERDICT_MERGED, VERDICT_NO_DEFECT, is_slice_row};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::{RunRow, StepRow};
use crate::pump::flow::outcome::{self, StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs::{make_read_only, step_replies};
use crate::pump::flow::verify_inputs::{
    admitted_plan, audit_context, branch_range, capped, cto_ruling, latest_of_class, rung_report,
};
use crate::pump::jobs::JobStore;
use crate::pump::store::PumpStore;
use crate::pump::store::step_payload::StepPayloadRow;
use vivi_error::VivariumError;
use vivi_mail::letter::roles::RoleKind;

/// The build node this handler overrides.
const REWORK_NODE: &str = "rework1";
/// The pool job kind of a rework slicer.
const JOB_KIND: &str = "flow-rework";
/// Most bytes of the admitted plan in the brief.
const PLAN_CAP: usize = 60_000;
/// Most bytes of a failed slice's last hand message in the brief.
const FAILED_MESSAGE_CAP: usize = 1_500;
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
         requirement gaps, failing rung output and slices that did not land below. Do not \
         re-cut merged slices.\n\n",
    );
    text.push_str("## Admitted plan\n\n");
    match plan {
        Some(plan) => text.push_str(&capped(&plan, PLAN_CAP)),
        None => text.push_str("(no admitted plan recorded)"),
    }
    let slices = branch_slices(cx, need, context_run)?;
    text.push_str("\n\n## Slices already merged on the need branch\n\n");
    text.push_str(&merged_section(&slices));
    text.push_str("\n\n## Slices that did not land\n\n");
    text.push_str(&unlanded_section(cx, &slices));
    text.push_str("\n\n## Audit findings and requirement gaps\n\n");
    text.push_str(&audit_section(&rows));
    if let Some(reason) = latest_of_class(&rows, "judge").and_then(|row| front_str(row, "reason")) {
        text.push_str(&format!("\n\n## Judge reason\n\n{reason}"));
    }
    text.push_str("\n\n## Failing rung output\n\n");
    text.push_str(&rung_section(&rows));
    // A run that restarted at rework1 always carries the ruling that started
    // it, whether or not an earlier run reached an audit.
    if context_run != cx.run.run || cx.run.entry == REWORK_NODE {
        text.push_str("\n\n## CTO ruling\n\n");
        match cto_ruling(cx.store, need, cx.run.run)? {
            Some(ruling) => text.push_str(&ruling),
            None => text.push_str("(no CTO ruling; an operator unlock restarted the run)"),
        }
    }
    Ok(text)
}

/// Every slice row of the runs whose slices are on the kept need branch.
fn branch_slices(
    cx: &StartCx<'_>,
    need: &str,
    context_run: u32,
) -> Result<Vec<StepRow>, VivariumError> {
    let mut runs = branch_runs(cx.store, cx.run)?;
    if !runs.contains(&context_run) {
        runs.push(context_run);
    }
    let mut steps = Vec::new();
    for run in runs {
        steps.extend(cx.store.steps_of(need, run)?);
    }
    Ok(steps.into_iter().filter(is_slice_row).collect())
}

fn landed(row: &StepRow) -> bool {
    matches!(
        row.verdict.as_deref(),
        Some(VERDICT_MERGED | VERDICT_NO_DEFECT)
    )
}

fn merged_section(slices: &[StepRow]) -> String {
    let lines = slices
        .iter()
        .filter(|row| landed(row))
        .map(|row| format!("- {} (task {})", row.node, row.task))
        .collect();
    none_if_empty(lines)
}

/// The slices that ended without landing (failed, or superseded by the
/// restart), each with the last message its hand left: the reason the rework
/// has to answer. A slice a later run landed under the same node is merged,
/// not missing: listing it here too would have the slicer cut it again.
fn unlanded_section(cx: &StartCx<'_>, slices: &[StepRow]) -> String {
    let merged: Vec<&str> = slices
        .iter()
        .filter(|row| landed(row))
        .map(|row| row.node.as_str())
        .collect();
    let mut lines = Vec::new();
    for row in slices
        .iter()
        .filter(|row| !landed(row) && !merged.contains(&row.node.as_str()))
    {
        let verdict = row.verdict.as_deref().unwrap_or("open");
        lines.push(format!("### {} (task {}, {verdict})", row.node, row.task));
        // Best effort: an unreadable task leaves the slice listed, unexplained.
        let message = match step_replies(cx.mail, &row.task) {
            Ok(replies) => replies.into_iter().next(),
            Err(error) => Some(format!("(its task could not be read: {error})")),
        };
        match message.filter(|text| !text.trim().is_empty()) {
            Some(text) => lines.push(format!(
                "Last hand message:\n{}",
                capped(&text, FAILED_MESSAGE_CAP)
            )),
            None => lines.push(String::from("(no hand message recorded)")),
        }
    }
    none_if_empty(lines)
}

/// This run and every earlier run it continued through a rework restart. A
/// rework restart keeps the need branch, so the slices those runs merged are
/// all on the branch this run reworks, audited or not.
fn branch_runs(store: &PumpStore, run: &RunRow) -> Result<Vec<u32>, VivariumError> {
    let rows = store.runs_of(&run.need)?;
    let mut runs = vec![run.run];
    let mut current = run.run;
    let mut entry = run.entry.clone();
    while entry == REWORK_NODE && current > 1 {
        current -= 1;
        runs.push(current);
        entry = rows
            .iter()
            .find(|row| row.run == current)
            .map(|row| row.entry.clone())
            .unwrap_or_default();
    }
    Ok(runs)
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
