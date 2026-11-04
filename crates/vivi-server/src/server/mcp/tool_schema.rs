//! `tools/list` entries that outgrew the [`super::tools_list`] literal, each
//! built with the module's existing [`tool`] constructor: the `send` tool
//! (with its hosted `dispatch` and `priority` options) and `pump_priority`.

use serde_json::{Value, json};

use super::tool;

/// The `send` tool: the ordinary send fields, plus the hosted dispatch
/// options — `dispatch` files one open pump record for the need besides
/// delivering it, and `priority` (which requires dispatch) stores a
/// dispatch level at the pump home.
pub(super) fn send_tool() -> Value {
    tool(
        "send",
        "Send a need, task, want, chat, or mail as the authenticated caller. The server composes the letter and mints its id. For kind=chat, to is optional. For a need, task, or want the sugar arguments build the letter's front matter as the CLI's flags do (a need needs done_when, a want needs question; a missing one is refused with vivi's message). A body that opens with +++ is the whole letter, and a sugar argument beside it is refused. Mail and chat take no sugar arguments. With dispatch true (hosted projects only, kind need only) the need keeps every delivery field and the ordinary send response, and the server also files one open pump record for it: the record rides the response as `pump`, and `priority` only when requested. Dispatch is authorized exactly like pump_send: an API key minted by the host's own member and acting as a role of kind operator; a member token or any other key is refused. priority requires dispatch: larger levels are more urgent, and 0 means none.",
        json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": ["need", "task", "want", "chat", "mail"] },
                "to": {
                    "oneOf": [
                        { "type": "string" },
                        { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                    ]
                },
                "subject": { "type": "string" },
                "body": { "type": "string", "description": "The prose, or the whole letter when it opens with +++" },
                "done_when": {
                    "description": "need (required there) or task: what must be true for the work to be done",
                    "oneOf": [
                        { "type": "string" },
                        { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                    ]
                },
                "target": { "type": "string", "description": "need or task: the repo or checkout the work targets" },
                "write_scope": {
                    "description": "need or task: paths the work may change",
                    "oneOf": [
                        { "type": "string" },
                        { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                    ]
                },
                "serves": { "type": "string", "description": "need: the want this need serves" },
                "explores": { "type": "string", "description": "need: the want this research need explores" },
                "question": { "type": "string", "description": "want (required there): the question or goal the want explores" },
                "depends_on": {
                    "description": "need, task, or want: handles of the items this one waits for; rides in the letter's front matter",
                    "oneOf": [
                        { "type": "string" },
                        { "type": "array", "items": { "type": "string" } }
                    ]
                },
                "dispatch": { "type": "boolean", "default": false, "description": "Hosted projects and kind need only: the server also files one open pump record for the need, keeping every delivery field and the ordinary response. Authorized like pump_send: an API key minted by the host's own member acting as a role of kind operator." },
                "priority": { "type": "integer", "minimum": 0, "maximum": i64::MAX, "description": "Requires dispatch. The need's dispatch priority: larger levels are more urgent, and 0 means none. Returned in the response only when requested." }
            },
            "required": ["kind", "to", "subject", "body"],
            "additionalProperties": false
        }),
    )
}

/// The `pump_priority` tool: operator-only per-need dispatch priority —
/// set, clear, or list the levels the running pump reads each tick.
pub(super) fn pump_priority_tool() -> Value {
    tool(
        "pump_priority",
        "Hosted projects only, operator only. Set, clear, or list one need's dispatch priority, as `vivi pump priority set|clear|list` do: a level per need in the pump home's need_priority table, which the running pump reads each tick. `action` is required: set stores `need` at `level` and returns {action, need, level}; clear removes the row and returns {action, need, level: 0}; list returns {priorities: [{need, level}...]}, highest level first, then by need handle. `need` is required for set and clear, `level` for set; contradictory arguments are refused: set without level, a level beside clear, and need or level beside list, as is an unknown action. A larger level is more urgent; 0 removes the row. Setting a level on a need that already has one replaces it — one row per canonical need key, the same key a dispatch send files its priority under — so reprioritizing rewrites the pump's existing row. The need key stays literal: it is never looked up, canonicalized, or required to exist, so a priority can be pre-seeded for a need that is only sent later. Every action, list included, is operator-only: same authorization as pump_control, the operator's API key minted by the host's own member and acting as a role of kind operator. set and clear are rate limited as writes; list is a read.",
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["set", "clear", "list"] },
                "need": { "type": "string", "description": "The need handle; required for set and clear" },
                "level": { "type": "integer", "minimum": 0, "maximum": i64::MAX, "description": "The priority level; required for set, refused beside clear and list" }
            },
            "required": ["action"],
            "additionalProperties": false
        }),
    )
}
