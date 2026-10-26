//! Shared readers for role-step briefs: pool replies, the need thread, targets.
//!
//! The planner, admitter, slicer, fanout and reslice handlers build their
//! briefs from the same few inputs; they live here once.

use serde_json::Value;

use crate::error::VivariumError;
use crate::pump::attempt_record::hand_message;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::flow::def::{FlowDef, NodeClass};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::outcome::role_job;
use crate::pump::flow::slice_dispatch::SliceMail;
use crate::pump::host::Ctx;
use crate::pump::jobs::JobSpec;
use crate::pump::store::PumpStore;

/// A pool worker's result reply starts with this text.
pub const POOL_RESULT_MARKER: &str = "pool result ";

/// The longest need thread kept, in bytes.
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

/// A need thread read once: the rendered brief text and the identities of
/// the messages that text actually carries. Nothing later that grows the
/// thread can retroactively join this record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedThread {
    /// The `handle: body` lines of the thread, cut between messages at
    /// [`THREAD_CAP`] bytes with a `[thread cut]` marker.
    pub text: String,
    /// The identity of every message whose line is in `text`, in thread
    /// order. A message the cut leaves out is never named here.
    pub included: Vec<String>,
}

/// The thread of `need` and the message identities it includes, from one
/// `need show --json` read, so the rendered text and the record of what it
/// supplied can never drift apart. A message's identity is its
/// `message_id`, or its mailspace handle when it has no Message-ID.
///
/// # Errors
/// Returns [`VivariumError`] when the thread cannot be read as messages.
pub fn need_thread(ctx: &Ctx, need: &str) -> Result<NeedThread, VivariumError> {
    let messages = crate::pump::flow::verify_inputs::need_thread(ctx, need)?;
    Ok(render_thread(&messages))
}

/// The thread as `handle: body` lines, each paired with the identity of the
/// message the line carries. Messages without a handle render nothing.
#[must_use]
pub(crate) fn thread_lines(messages: &[Value]) -> Vec<(String, String)> {
    messages
        .iter()
        .filter_map(|message| {
            let handle = message["handle"].as_str()?;
            let identity = message["message_id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .unwrap_or(handle);
            Some((
                identity.to_owned(),
                format!(
                    "{handle}: {}",
                    message["body"].as_str().unwrap_or("").trim()
                ),
            ))
        })
        .collect()
}

/// Render the thread lines, cut between messages at [`THREAD_CAP`] bytes,
/// naming only the messages whose whole line the text keeps.
#[must_use]
pub(crate) fn render_thread(messages: &[Value]) -> NeedThread {
    let mut text = String::new();
    let mut included = Vec::new();
    for (identity, line) in thread_lines(messages) {
        if !text.is_empty() {
            text.push('\n');
        }
        if text.len() + line.len() > THREAD_CAP {
            text.push_str("[thread cut]");
            break;
        }
        text.push_str(&line);
        included.push(identity);
    }
    NeedThread { text, included }
}

/// The job payload key holding the message identities a role request's
/// brief actually carried; absent on historical jobs.
pub const INPUT_IDENTITIES: &str = "input_identities";

/// Stamp `identities` into `payload` as the message identities the request's
/// brief carried. Rerun, budget-retry and correction copies keep the stamp
/// because they copy the payload. Nothing is stamped for an empty list, so
/// an absent key always reads as "no record", never as "no inputs".
pub fn stamp_input_identities(payload: &mut serde_json::Value, identities: &[String]) {
    if identities.is_empty() {
        return;
    }
    if !payload.is_object() {
        *payload = serde_json::Value::Object(serde_json::Map::new());
    }
    if let Some(map) = payload.as_object_mut() {
        map.insert(
            INPUT_IDENTITIES.to_owned(),
            serde_json::Value::Array(
                identities
                    .iter()
                    .map(|identity| serde_json::Value::String(identity.clone()))
                    .collect(),
            ),
        );
    }
}

/// The message identities `payload` says its brief carried; empty when the
/// job predates the stamp.
#[must_use]
pub fn input_identities(payload: &serde_json::Value) -> Vec<String> {
    payload
        .get(INPUT_IDENTITIES)
        .and_then(serde_json::Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
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
