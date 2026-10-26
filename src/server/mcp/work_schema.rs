//! `tools/list` entries for the hosted operator tools that outgrew the
//! [`super::tools_list`] literal, each built with the module's existing
//! [`tool`] constructor: the `pump_send` and `pump_action` writes, and the
//! operator-only mind reads — `need_show`, `need_thread` and `task_thread`.

use serde_json::{Value, json};

use super::tool;

/// The `pump_send` tool: one open pump record handed to the pump for a need.
///
/// The pump record is addressed to the pump identity; the need itself need
/// not be addressed to the pump, so a need addressed to any role dispatches
/// the same way.
pub(super) fn pump_send_tool() -> Value {
    tool(
        "pump_send",
        "Hosted projects only, operator only. Hand a need to the pump: file one open pump record for it, as `vivi pump send --need <handle>` does; the pump reads the record on its next tick. The need itself need not be addressed to the pump — it is the record that goes to the pump identity — so a need addressed to any role dispatches the same way. Same authorization as pump_control: the operator's API key, minted by the host's own member and acting as a role of kind operator. Refused for an unknown handle, anything that is not a need, a need that is done, or a need that already has an open record (the error names it). Rate limited as a write. Returns the record.",
        json!({
            "type": "object",
            "properties": {
                "need": { "type": "string", "description": "The need handle the dispatch covers" },
                "body": { "type": "string", "description": "Record body, literal text (the CLI's - and @path forms are not accepted). Defaults to a dispatch line." },
                "from": { "type": "string", "description": "Sender identity (the CLI's --from); defaults to the project's [pump] from" }
            },
            "required": ["need"],
            "additionalProperties": false
        }),
    )
}

/// The `pump_action` tool: the pump's library actions as one tool call.
pub(super) fn pump_action_tool() -> Value {
    tool(
        "pump_action",
        "Hosted projects only, operator only. One pump action, the same library call as the CLI: withdraw (needs + reason; closes each need withdrawn), requeue (steps or job, not both; dry_run previews), replan (need), answer (step + word, note optional), or unlock (need, entry and note optional). Posted for the pump's next tick. Not refused while the pump runs. A library refusal is the tool error, with the library's message. Every call is a write and one audit line, including a requeue dry run. Same authorization as pump_control.",
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["withdraw", "requeue", "replan", "answer", "unlock"] },
                "need": { "type": "string", "description": "Need handle for replan and unlock" },
                "needs": {
                    "description": "Need handles to withdraw, as a string or an array of strings",
                    "oneOf": [
                        { "type": "string" },
                        { "type": "array", "items": { "type": "string" } }
                    ]
                },
                "steps": {
                    "description": "Flow step task handles to requeue, as a string or an array of strings",
                    "oneOf": [
                        { "type": "string" },
                        { "type": "array", "items": { "type": "string" } }
                    ]
                },
                "job": { "type": "string", "description": "Held pool job to release. Mutually exclusive with steps" },
                "step": { "type": "string", "description": "Flow step task to answer" },
                "word": { "type": "string", "description": "Answer word" },
                "entry": { "type": "string", "description": "Flow entry to restart at (unlock). The flow's first entry when omitted" },
                "reason": { "type": "string", "description": "Withdraw reason, or the reason posted with a step requeue" },
                "note": { "type": "string", "description": "Letter body for answer and unlock" },
                "dry_run": { "type": "boolean", "default": false, "description": "Requeue only. Preview without posting. Still audited as a write" }
            },
            "required": ["action"],
            "additionalProperties": false
        }),
    )
}

/// The `need_show` tool: one need's full read for the operator.
pub(super) fn need_show_tool() -> Value {
    tool(
        "need_show",
        "Hosted projects only, operator only. Show one need by handle, as the member `show` answers for it: the item row (handle, message id, kind, role, marks, status, dependencies, last event), the rendered text, and the parsed letter. An unknown handle is refused, as is a handle that is not a need. Same authorization as pump_control.",
        json!({
            "type": "object",
            "properties": {
                "handle": { "type": "string", "description": "The need handle to show" }
            },
            "required": ["handle"],
            "additionalProperties": false
        }),
    )
}

/// The `need_thread` tool: one need's thread, bounded and paged.
pub(super) fn need_thread_tool() -> Value {
    thread_tool(
        "need_thread",
        "need",
        "Hosted projects only, operator only. The reply thread around one need: the need's state and newest lifecycle event, then the thread's letters oldest-first, each with its handle, kind, from/to, subject, role, date, marks, status and body. At most `limit` letters are returned (default 20, at most 100); `before` is a paging cursor — only letters before that thread handle are returned. An unknown handle is refused. Same authorization as pump_control.",
    )
}

/// The `task_thread` tool: one task's thread, bounded and paged.
pub(super) fn task_thread_tool() -> Value {
    thread_tool(
        "task_thread",
        "task",
        "Hosted projects only, operator only. The reply thread around one task: the task's state and newest lifecycle event, then the thread's letters oldest-first, each with its handle, kind, from/to, subject, role, date, marks, status and body. At most `limit` letters are returned (default 20, at most 100); `before` is a paging cursor — only letters before that thread handle are returned. An unknown handle is refused. Same authorization as pump_control.",
    )
}

/// The shared `need_thread` / `task_thread` schema: one work item's thread
/// with a bounded page and a `before` cursor.
fn thread_tool(name: &str, what: &str, description: &str) -> Value {
    tool(
        name,
        description,
        json!({
            "type": "object",
            "properties": {
                "handle": { "type": "string", "description": format!("The {what} handle to read") },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20, "description": "Letters returned" },
                "before": { "type": "string", "description": "Return only letters before this thread handle (paging)" }
            },
            "required": ["handle"],
            "additionalProperties": false
        }),
    )
}
