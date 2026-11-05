//! Recipient parsing for the MCP writes that name an audience.

use serde_json::Value;

use super::ToolError;

/// The `to` audience. Omission and an empty array are an empty audience;
/// every supplied entry must be a non-blank address.
pub(in crate::server::mcp) fn parse_to(arguments: &Value) -> Result<Vec<String>, ToolError> {
    match arguments.get("to") {
        None => Ok(Vec::new()),
        Some(Value::String(raw)) => {
            let list: Result<Vec<_>, _> = raw.split(',').map(address_entry).collect();
            list
        }
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .ok_or_else(|| ToolError::new("to must be strings"))
                    .and_then(address_entry)
            })
            .collect(),
        Some(_) => Err(ToolError::new("to must be a string or an array of strings")),
    }
}

/// One supplied address, trimmed. A blank entry is malformed, not omitted.
fn address_entry(raw: &str) -> Result<String, ToolError> {
    let address = raw.trim();
    if address.is_empty() {
        Err(ToolError::new("to entries must be non-blank addresses"))
    } else {
        Ok(address.to_owned())
    }
}

#[cfg(test)]
#[path = "addressing_test.rs"]
mod tests;
