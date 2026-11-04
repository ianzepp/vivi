//! Shared parsing for the send dispatch options (`dispatch`, `priority`).
//!
//! Both are optional send arguments accepted alongside the ordinary send
//! fields; [`SendOptions::parse`] reads and validates them, and
//! [`non_negative_integer`] is the whole-number validator behind the
//! priority level, reusable wherever a non-negative level is read. Routing
//! a parsed send is the caller's concern, not this module's.

use serde_json::Value;

use super::tools::ToolError;

/// The parsed `dispatch` and `priority` send arguments: whether the letter
/// routes through the dispatch pump, and the non-negative priority level
/// to file it at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct SendOptions {
    pub dispatch: bool,
    pub priority: Option<i64>,
}

impl SendOptions {
    /// Parse `dispatch` and `priority` out of send tool arguments.
    ///
    /// An omitted or null `dispatch` is false — the same rule
    /// `live::role_write::opt_bool` applies; every other non-boolean
    /// fails. An omitted `priority` is none; a supplied one must be a JSON
    /// integer in 0..=i64::MAX, so null, strings, booleans, floats (even
    /// 1.0), negatives, arrays, objects and overflow all fail. Presence
    /// matters: a priority of zero still files at zero, so it needs
    /// `dispatch` too. A dispatch only ever routes a `need`, the one kind
    /// the pump settles, so any other or missing `kind` fails when
    /// `dispatch` is set. Unrelated ordinary-send arguments are not
    /// inspected, and no backend is chosen here.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn parse(arguments: &Value) -> Result<Self, ToolError> {
        let dispatch = match arguments.get("dispatch") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => return Err(ToolError::new("dispatch must be true or false")),
        };
        let priority = match arguments.get("priority") {
            None => None,
            Some(value) => Some(non_negative_integer(value, "priority")?),
        };
        if priority.is_some() && !dispatch {
            return Err(ToolError::new("priority requires dispatch"));
        }
        if dispatch && arguments.get("kind").and_then(Value::as_str) != Some("need") {
            return Err(ToolError::new("dispatch is only supported for kind need"));
        }
        Ok(Self { dispatch, priority })
    }
}

/// A supplied JSON integer in 0..=i64::MAX, for non-negative levels such
/// as the send priority. Null, strings, booleans, floats (even 1.0),
/// negatives, arrays, objects and integers past `i64::MAX` all fail.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn non_negative_integer(value: &Value, name: &str) -> Result<i64, ToolError> {
    match value.as_i64() {
        Some(level) if level >= 0 => Ok(level),
        _ => Err(ToolError::new(format!(
            "{name} must be a whole number of 0 or more"
        ))),
    }
}

#[cfg(test)]
#[path = "send_options_test.rs"]
mod tests;
