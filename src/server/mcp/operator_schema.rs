//! `tools/list` entries for the operator-only hosted tools that outgrew the
//! [`super::tools_list`] literal: `pump_action` (moved unchanged) and
//! `need_units`, each built with the module's existing [`tool`] constructor.

use serde_json::{Value, json};

use super::tool;

/// The `pump_action` tool, verbatim from `tools_list`: one operator pump
/// action, the same library call as the CLI.
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

/// The `need_units` tool: read-only page over the units bound to a need in
/// the hosted backlog graph.
pub(super) fn need_units_tool() -> Value {
    tool(
        "need_units",
        "Hosted projects only, operator only. Read-only: the units bound to a need in the project's backlog graph — the tasks and wants `need bind` lowered onto it — each with its handle and graph node state. `need` resolves by handle or unambiguous prefix and must name a need: an unknown handle or another kind is refused, so an empty `units` always means a real need with no bound units. Paged: `limit` (default 20, at most 100) units from `offset` (default 0). Returns {need, units, total, offset, limit, next_offset, truncated}: `next_offset` is set only when a next page follows, and an offset at or past the end is the empty final page. A read: not audited and not rate limited. Same authorization as pump_control.",
        json!({
            "type": "object",
            "properties": {
                "need": { "type": "string", "description": "The need handle (or unambiguous prefix) whose bound units are listed" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20, "description": "Units returned per page" },
                "offset": { "type": "integer", "minimum": 0, "default": 0, "description": "First unit to return; 0 starts the list" }
            },
            "required": ["need"],
            "additionalProperties": false
        }),
    )
}
