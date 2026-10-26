//! Which server-endpoint project tool calls write (so [`super::project_tool`]
//! audits and rate limits them) and which only read. Extracted verbatim from
//! `server.rs`; the action-gated tools write only on their write actions.

use serde_json::Value;

/// Whether one tool call writes (so it is audited and rate limited). The
/// action-gated tools write only on their write actions.
pub(super) fn is_write(name: &str, arguments: &Value) -> bool {
    let action = arguments.get("action").and_then(Value::as_str);
    match name {
        "send" | "reply" | "done" | "reopen" | "mark" | "unmark" | "depend" | "pump_control"
        | "pump_send" | "pump_action" | "pump_reset" => true,
        "memo" => action == Some("save"),
        "pump_rungs" => action == Some("set"),
        "pool_bands" => matches!(action, Some("set" | "unset")),
        "pump_priority" => matches!(action, Some("set" | "clear")),
        "config_set" | "config_unset" | "pool_allow_set" | "target_add" | "target_remove"
        | "role_add" | "role_set" | "charter_set" => true,
        "config_migrate" => match arguments.get("write") {
            Some(Value::Bool(flag)) => *flag,
            None | Some(Value::Null) => false,
            Some(_) => true,
        },
        _ => false,
    }
}

#[cfg(test)]
#[path = "write_classification_test.rs"]
mod tests;
