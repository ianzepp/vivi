//! Turning one permitted action into its single server tool call: the
//! [`ActionRequest`] becomes a tool name and arguments, with the helpers
//! that name the project, need and step. One action, one call; the server
//! decides what the answer means.

use std::path::Path;

use serde_json::{Value, json};

use super::Call;
use crate::mind::actions::ActionRequest;
use crate::mind::authority::Action;

/// The one tool call the action makes.
pub(super) fn call_for(request: &ActionRequest) -> Result<Call, String> {
    let project = project_name(request)?;
    let note = request.note.as_deref();
    Ok(match request.kind {
        Action::PausePump => control(&project, "pause"),
        Action::ResumePump => control(&project, "resume"),
        Action::Requeue => pump_action(
            &project,
            json!({ "action": "requeue", "steps": [step_of(request)?], "reason": note }),
        ),
        Action::Replan => pump_action(
            &project,
            json!({ "action": "replan", "need": need_of(request)? }),
        ),
        Action::Answer => answer_call(&project, request)?,
        Action::Unlock => pump_action(
            &project,
            json!({ "action": "unlock", "need": need_of(request)?, "note": note }),
        ),
        Action::ReadStatus => read_status_call(&project, request),
        Action::ReadLog => read_log_call(&project, request)?,
        Action::LetterToOperator | Action::Escalate => letter(&project, request),
        Action::ReadNeed
        | Action::ReadNeedThread
        | Action::ReadTaskThread
        | Action::ReadFlow
        | Action::ReadConfig
        | Action::ReadPool
        | Action::ReplyNeed
        | Action::SetPriority
        | Action::WithdrawNeed
        | Action::SetRoleModel
        | Action::SetPoolCap
        | Action::SetPoolBand
        | Action::FileNeed
        | Action::FileFixNeed
        | Action::DispatchNeed
        | Action::Upgrade
        | Action::SelfRelease => return Err(not_wired(request.kind)),
    })
}

/// The failure text for an action the mapping slices have not wired to the
/// server's tools yet.
fn not_wired(kind: Action) -> String {
    format!("{} is not wired to the server yet", kind.as_str())
}

/// The server's name for a project: its root's directory name, which is how
/// a hosted project is named.
fn project_name(request: &ActionRequest) -> Result<String, String> {
    request
        .project
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .ok_or_else(|| String::from("the action names no project"))
}

fn need_of(request: &ActionRequest) -> Result<&str, String> {
    request
        .need
        .as_deref()
        .ok_or_else(|| String::from("the action names no need"))
}

fn step_of(request: &ActionRequest) -> Result<&str, String> {
    request
        .step
        .as_deref()
        .ok_or_else(|| String::from("the action names no step"))
}

/// The `pump_action` answer call for one step.
fn answer_call(project: &str, request: &ActionRequest) -> Result<Call, String> {
    Ok(pump_action(
        project,
        json!({
            "action": "answer",
            "step": step_of(request)?,
            "word": request.word.as_deref().ok_or("the action names no word")?,
            "note": request.note.as_deref(),
        }),
    ))
}

/// The status read: one `pump_list` over the optional need, status and
/// limit filters. Absent values are left out rather than sent as nulls, so
/// open steps stay the server's default when no status is named.
fn read_status_call(project: &str, request: &ActionRequest) -> Call {
    (
        "pump_list",
        without_unset(json!({
            "project": project,
            "need": request.need,
            "status": request.status,
            "limit": request.limit,
        })),
    )
}

/// The log read: one `pump_log` naming a required file, with the optional
/// line count and text filter left out when absent.
fn read_log_call(project: &str, request: &ActionRequest) -> Result<Call, String> {
    let file = request
        .file
        .as_deref()
        .ok_or_else(|| String::from("the action names no file"))?;
    Ok((
        "pump_log",
        without_unset(json!({
            "project": project,
            "file": file,
            "lines": request.lines,
            "filter": request.filter,
        })),
    ))
}

fn control(project: &str, action: &str) -> Call {
    (
        "pump_control",
        json!({ "project": project, "action": action }),
    )
}

/// `pump_action` arguments: the given object with unset (`null`) fields
/// left out, plus the project.
fn pump_action(project: &str, arguments: Value) -> Call {
    let mut arguments = without_unset(arguments);
    if let Some(object) = arguments.as_object_mut() {
        object.insert("project".into(), Value::String(project.to_string()));
    }
    ("pump_action", arguments)
}

/// The given object with unset (`null`) fields left out, so the server's
/// own defaults hold.
fn without_unset(mut arguments: Value) -> Value {
    if let Some(object) = arguments.as_object_mut() {
        object.retain(|_, value| !value.is_null());
    }
    arguments
}

fn letter(project: &str, request: &ActionRequest) -> Call {
    let subject = match request.need.as_deref() {
        Some(need) => format!("mind: {} (need {need})", request.kind.as_str()),
        None => format!("mind: {}", request.kind.as_str()),
    };
    (
        "send",
        json!({
            "project": project,
            "kind": "mail",
            "to": "operator",
            "subject": subject,
            "body": request.note.as_deref().unwrap_or_default(),
        }),
    )
}
