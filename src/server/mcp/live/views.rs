//! The D4 read tools: hosted-project views through the same vivi library
//! functions their CLI commands call. Every tool here is read-only — no
//! config edit, no record, no letter — and operator-only, held to
//! [`super::pump`]'s authorization like the other pump tools.
//!
//! The JSON envelopes are the CLI's `--json` bodies: `role_list` and
//! `pool_list` answer the row arrays, `role_show` and `pool_show` the row
//! objects, `config_validate` the issue array, and `config_show` the
//! overview (plus `origins` when values are inherited). `flow_show` has no
//! derived `Serialize` on its answer, so this leaf builds the JSON exactly
//! as the CLI's `flow show --json` does.
//!
//! Live free-slot counts are never resolved: the probe runs
//! `vivi pool status` through the current executable, which on the server is
//! the server binary — a `resolved: true` call is refused rather than
//! answered with a fabricated zero.

use std::path::Path;

use crate::pump::config;
use crate::pump::config::targets;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::registry::FLOW_VERSION;
use crate::pump::flow::view as flow_view;
use crate::pump::logview::{self, LogFile, LogFilter};
use serde_json::{Value, json};

use crate::server::identities::IdentityRecord;
use crate::server::mcp::tools::{ToolError, opt_str, opt_usize, req_str};
use crate::server::safety::RateLimiter;

use super::lib_error;
use super::pump::{acting_key, authorize};

/// The free-slot refusal `resolved: true` answers, on both tools that take it.
const RESOLVED_REFUSED: &str = "the server cannot resolve live free slots: the probe runs the \
     vivi binary's `pool status`, which the server is not; ask on the host instead";

pub(in crate::server::mcp) fn pump_log(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("pump_log", "read the pump log", acting)?;
    let file = match req_str(arguments, "file")? {
        "log" => LogFile::Log,
        "exceptions" => LogFile::Exceptions,
        other => {
            return Err(ToolError::new(format!(
                "file must be log or exceptions; got '{other}'"
            )));
        }
    };
    let lines = opt_usize(arguments, "lines", 50, 500)?;
    let filter = opt_str(arguments, "filter")?;
    authorize("pump_log", root, caller, requested)?;
    let word = if matches!(file, LogFile::Log) {
        "log"
    } else {
        "exceptions"
    };
    let tail =
        logview::view(root, file, lines, &LogFilter::new(None, filter)).map_err(lib_error)?;
    Ok(json!({ "file": word, "lines": tail }))
}

pub(in crate::server::mcp) fn flow_show(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("flow_show", "show the flow", acting)?;
    let kind = FlowKind::parse(req_str(arguments, "kind")?)
        .ok_or_else(|| ToolError::new("kind must be build, research, release, or repair"))?;
    let need = opt_str(arguments, "need")?;
    authorize("flow_show", root, caller, requested)?;
    let flow = flow_view(root, kind, need).map_err(lib_error)?;
    let mut out = json!({
        "kind": flow.kind.name(),
        "version": FLOW_VERSION,
        "mermaid": flow.mermaid,
    });
    if let Some(runs) = &flow.runs {
        out["runs"] = runs.as_deref().map_or(Value::Null, runs_json);
    }
    Ok(out)
}

pub(in crate::server::mcp) fn config_show(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("config_show", "show the config", acting)?;
    refuse_resolved(arguments)?;
    authorize("config_show", root, caller, requested)?;
    let report = config::show(Some(root), false).map_err(lib_error)?;
    // The CLI's `--json` body: the overview, plus `origins` only when some
    // merged value was inherited.
    let mut value = serde_json::to_value(&report.overview)
        .map_err(|error| ToolError::new(format!("cannot encode the config: {error}")))?;
    if report.origins.inherited()
        && let Some(object) = value.as_object_mut()
    {
        let origins = serde_json::to_value(&report.origins)
            .map_err(|error| ToolError::new(format!("cannot encode the origins: {error}")))?;
        object.insert("origins".into(), origins);
    }
    Ok(value)
}

pub(in crate::server::mcp) fn config_validate(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    _arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("config_validate", "validate the config", acting)?;
    authorize("config_validate", root, caller, requested)?;
    let report = config::validate(Some(root)).map_err(lib_error)?;
    Ok(config::issues_json(&report.issues))
}

pub(in crate::server::mcp) fn pool_list(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    _arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("pool_list", "list the pools", acting)?;
    authorize("pool_list", root, caller, requested)?;
    let rows = crate::pool::config::list_view(Some(root)).map_err(lib_error)?;
    serde_json::to_value(&rows)
        .map_err(|error| ToolError::new(format!("cannot encode the pools: {error}")))
}

pub(in crate::server::mcp) fn pool_show(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("pool_show", "show a pool", acting)?;
    let name = req_str(arguments, "name")?;
    refuse_resolved(arguments)?;
    authorize("pool_show", root, caller, requested)?;
    let view = crate::pool::config::show_view(Some(root), name, false).map_err(lib_error)?;
    serde_json::to_value(&view.row)
        .map_err(|error| ToolError::new(format!("cannot encode the pool: {error}")))
}

pub(in crate::server::mcp) fn role_list(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    _arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("role_list", "list the roles", acting)?;
    let hosted = authorize("role_list", root, caller, requested)?;
    let views = hosted.ms.list_role_views().map_err(lib_error)?;
    serde_json::to_value(&views)
        .map_err(|error| ToolError::new(format!("cannot encode the roles: {error}")))
}

pub(in crate::server::mcp) fn role_show(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("role_show", "show a role", acting)?;
    let role = req_str(arguments, "role")?;
    let hosted = authorize("role_show", root, caller, requested)?;
    let view = hosted.ms.role_view(role).map_err(lib_error)?;
    serde_json::to_value(&view)
        .map_err(|error| ToolError::new(format!("cannot encode the role: {error}")))
}

pub(in crate::server::mcp) fn charter_show(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("charter_show", "read a charter", acting)?;
    let role = req_str(arguments, "role")?;
    let hosted = authorize("charter_show", root, caller, requested)?;
    // `read_charter` takes the canonical name; `role_view` resolves the
    // alias first, and the charter JSON rides the same envelope.
    let canonical = hosted.ms.resolve_identity(role).map_err(lib_error)?;
    let charter = hosted.ms.read_charter(&canonical).map_err(lib_error)?;
    Ok(json!({
        "name": canonical,
        "address": hosted.ms.address_for(&canonical),
        "has_charter": !charter.is_empty(),
        "charter": charter,
    }))
}

pub(in crate::server::mcp) fn target_list(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    _arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("target_list", "list the pump targets", acting)?;
    authorize("target_list", root, caller, requested)?;
    let rows = targets::list(root).map_err(lib_error)?;
    Ok(Value::Array(
        rows.iter()
            .map(|row| {
                json!({
                    "id": row.id,
                    "path": row.path,
                    "main": row.main,
                    "rungs": row.rungs,
                })
            })
            .collect(),
    ))
}

/// Refuse `resolved: true`: the live free-slot probe is the vivi binary's,
/// and the server must not answer a fabricated count.
fn refuse_resolved(arguments: &Value) -> Result<(), ToolError> {
    match arguments.get("resolved") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(()),
        Some(Value::Bool(true)) => Err(ToolError::new(RESOLVED_REFUSED)),
        Some(_) => Err(ToolError::new("resolved must be true or false")),
    }
}

/// The runs array of `flow show --json`, built as the CLI builds it.
fn runs_json(runs: &[(RunRow, Vec<StepRow>)]) -> Value {
    Value::Array(
        runs.iter()
            .map(|(run, steps)| {
                json!({
                    "run": run.run,
                    "flow": run.flow,
                    "flow_version": run.flow_version,
                    "entry": run.entry,
                    "state": run.state,
                    "steps": steps.iter().map(|step| json!({
                        "node": step.node,
                        "task": step.task,
                        "state": state_word(step.state),
                        "verdict": step.verdict,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

/// The step-state word the CLI prints.
fn state_word(state: StepState) -> &'static str {
    match state {
        StepState::Pending => "pending",
        StepState::Running => "running",
        StepState::Waiting => "waiting",
        StepState::Done => "done",
        StepState::Failed => "failed",
    }
}
