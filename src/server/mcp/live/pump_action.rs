//! `pump_action` and `pump_reset`: operator pump actions on a hosted project.
//!
//! Each call is the library function the CLI uses. `pump_action` posts for
//! the next tick and does not look at the pump lock. `pump_reset` is
//! [`crate::pump::reset::archive`], which refuses while that lock is held
//! (a paused pump holds it too).

use std::path::Path;

use crate::pump::requeue::{RequeueResult, RequeueTarget};
use crate::pump::withdraw::WithdrawClose;
use serde_json::{Value, json};

use crate::server::identities::IdentityRecord;
use crate::server::mcp::tools::{ToolError, rate_limit, req_str};
use crate::server::safety::RateLimiter;

use super::lib_error;
use super::pump::{acting_key, authorize};

/// Withdraw, requeue, replan, answer, or unlock. Rate limited as a write,
/// including a requeue dry run.
pub(in crate::server::mcp) fn pump_action(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("pump_action", "take a pump action", acting)?;
    let action = req_str(arguments, "action")?;
    if !matches!(
        action,
        "withdraw" | "requeue" | "replan" | "answer" | "unlock"
    ) {
        return Err(ToolError::new(format!(
            "action must be withdraw, requeue, replan, answer, or unlock; got '{action}'"
        )));
    }
    let hosted = authorize("pump_action", root, caller, requested)?;
    rate_limit(limiter, project_id, &hosted.actor)?;
    drop(hosted);
    match action {
        "withdraw" => withdraw(root, arguments),
        "requeue" => requeue(root, arguments),
        "replan" => replan(root, arguments),
        "answer" => answer(root, arguments),
        "unlock" => unlock(root, arguments),
        _ => unreachable!("action was checked above"),
    }
}

/// Archive the pump home and start a fresh database. Refused while the pump
/// lock is held, with the library's message.
pub(in crate::server::mcp) fn pump_reset(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    _arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("pump_reset", "reset the pump", acting)?;
    let hosted = authorize("pump_reset", root, caller, requested)?;
    rate_limit(limiter, project_id, &hosted.actor)?;
    drop(hosted);
    let report = crate::pump::reset::archive(root).map_err(lib_error)?;
    Ok(json!({
        "archive_dir": report.archive_dir.display().to_string(),
        "moved": report.moved,
        "mode": report.mode.to_string(),
        "open_records": report.open_records,
    }))
}

fn withdraw(root: &Path, arguments: &Value) -> Result<Value, ToolError> {
    let needs = super::string_list(arguments, "needs")?;
    if needs.is_empty() {
        return Err(ToolError::new("withdraw needs at least one need"));
    }
    let reason = raw_str(arguments, "reason")?.unwrap_or("");
    let posted = crate::pump::withdraw::withdraw(root, &needs, reason).map_err(lib_error)?;
    let results = posted
        .iter()
        .map(|row| {
            json!({
                "need": row.need,
                "close": close_word(row.close),
                "record": row.record,
                "id": row.id,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({"action": "withdraw", "results": results}))
}

fn requeue(root: &Path, arguments: &Value) -> Result<Value, ToolError> {
    let steps = super::string_list(arguments, "steps")?;
    let job = raw_str(arguments, "job")?;
    let dry_run = flag(arguments, "dry_run")?;
    if job.is_some() && !steps.is_empty() {
        return Err(ToolError::new("requeue takes steps or job, not both"));
    }
    let reason = raw_str(arguments, "reason")?;
    let target = match job {
        Some(id) => RequeueTarget::Job(id),
        None => RequeueTarget::Steps {
            tasks: &steps,
            reason,
        },
    };
    let posted = crate::pump::requeue::requeue(root, &target, dry_run).map_err(lib_error)?;
    let results = posted.iter().map(requeue_row).collect::<Vec<_>>();
    Ok(json!({
        "action": "requeue",
        "dry_run": dry_run,
        "results": results,
    }))
}

fn replan(root: &Path, arguments: &Value) -> Result<Value, ToolError> {
    let need = required_raw(arguments, "need")?;
    let posted = crate::pump::replan::replan(root, need).map_err(lib_error)?;
    Ok(json!({
        "action": "replan",
        "need": posted.need,
        "record": posted.record,
        "id": posted.id,
    }))
}

fn answer(root: &Path, arguments: &Value) -> Result<Value, ToolError> {
    let step = required_raw(arguments, "step")?;
    let word = required_raw(arguments, "word")?;
    let note = raw_str(arguments, "note")?;
    let posted = crate::pump::flow::answer::answer(root, step, word, note).map_err(lib_error)?;
    Ok(json!({
        "action": "answer",
        "step": posted.step,
        "word": posted.word,
        "need": posted.need,
        "record": posted.record,
        "id": posted.id,
    }))
}

fn unlock(root: &Path, arguments: &Value) -> Result<Value, ToolError> {
    let need = required_raw(arguments, "need")?;
    let entry = raw_str(arguments, "entry")?;
    let note = raw_str(arguments, "note")?;
    let posted = crate::pump::unlock::unlock(root, need, entry, note).map_err(lib_error)?;
    Ok(json!({
        "action": "unlock",
        "need": posted.need,
        "record": posted.record,
        "id": posted.id,
        "entry": posted.entry,
    }))
}

fn requeue_row(result: &RequeueResult) -> Value {
    match result {
        RequeueResult::Step {
            task,
            need,
            record,
            id,
        } => json!({
            "kind": "step",
            "task": task,
            "need": need,
            "record": record,
            "id": id,
        }),
        RequeueResult::StepPreview { task, need } => json!({
            "kind": "step_preview",
            "task": task,
            "need": need,
        }),
        RequeueResult::Job { id } => json!({"kind": "job", "id": id}),
        RequeueResult::JobPreview { id } => json!({"kind": "job_preview", "id": id}),
    }
}

fn close_word(close: WithdrawClose) -> &'static str {
    match close {
        WithdrawClose::Closed => "closed",
        WithdrawClose::AlreadyClosed => "already_closed",
    }
}

/// A string argument as given, including blank. Absent is `None`.
fn raw_str<'a>(arguments: &'a Value, name: &str) -> Result<Option<&'a str>, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.as_str())),
        Some(_) => Err(ToolError::new(format!("{name} must be a string"))),
    }
}

/// `name` must be present. A blank string is returned so the library can
/// refuse it with its own message.
fn required_raw<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, ToolError> {
    raw_str(arguments, name)?.ok_or_else(|| ToolError::new(format!("{name} is required")))
}

fn flag(arguments: &Value, name: &str) -> Result<bool, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(ToolError::new(format!("{name} must be true or false"))),
    }
}
