//! Codex SSE allowlist and `response.completed.output` rebuild.

use serde_json::{Value, json};

use super::sse::Event;

/// Event types forwarded to the client. Everything else is dropped.
pub const ALLOWLIST: &[&str] = &[
    "response.created",
    "response.in_progress",
    "response.output_item.added",
    "response.content_part.added",
    "response.output_text.delta",
    "response.output_text.done",
    "response.function_call_arguments.delta",
    "response.function_call_arguments.done",
    "response.content_part.done",
    "response.output_item.done",
    "response.completed",
    "response.failed",
    "response.incomplete",
    "error",
];

/// Accumulator used while a Responses stream is rewritten.
#[derive(Debug, Default)]
pub struct Rebuild {
    items: Vec<Value>,
}

impl Rebuild {
    /// Remember a `response.output_item.done` item (latest index wins).
    pub fn push_item(&mut self, item: Value) {
        let index = item.get("id").and_then(Value::as_str).map(str::to_owned);
        if let Some(id) = index
            && let Some(existing) = self
                .items
                .iter_mut()
                .find(|row| row.get("id").and_then(Value::as_str) == Some(id.as_str()))
        {
            *existing = item;
            return;
        }
        self.items.push(item);
    }

    /// Items collected so far.
    #[must_use]
    pub fn items(&self) -> &[Value] {
        &self.items
    }
}

/// Filter one SSE event. Returns `None` to drop it.
pub fn filter(event: Event, rebuild: &mut Rebuild) -> Option<Event> {
    let Some(data) = event.data() else {
        return Some(event);
    };
    if data == "[DONE]" {
        return Some(event);
    }
    let Ok(mut value) = serde_json::from_str::<Value>(&data) else {
        return Some(event);
    };
    let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    if !event_type.is_empty() && !ALLOWLIST.contains(&event_type) {
        return None;
    }
    if event_type == "response.output_item.done"
        && let Some(item) = value.get("item").cloned()
    {
        rebuild.push_item(item);
    }
    if event_type == "response.completed" {
        rebuild_completed(&mut value, rebuild);
        return Some(replace_data(&event, &value));
    }
    Some(event)
}

fn rebuild_completed(value: &mut Value, rebuild: &Rebuild) {
    let Some(response) = value.get_mut("response") else {
        return;
    };
    let empty = match response.get("output") {
        None | Some(Value::Null) => true,
        Some(Value::Array(items)) => items.is_empty(),
        Some(_) => false,
    };
    if empty && !rebuild.items.is_empty() {
        response["output"] = json!(rebuild.items.clone());
    }
}

fn replace_data(event: &Event, value: &Value) -> Event {
    let Ok(data) = serde_json::to_string(value) else {
        return event.clone();
    };
    let mut lines: Vec<String> = event
        .raw
        .lines()
        .filter(|line| !line.starts_with("data:") && !line.trim().is_empty())
        .map(str::to_owned)
        .collect();
    lines.push(format!("data: {data}"));
    Event {
        raw: format!("{}\n\n", lines.join("\n")),
    }
}

/// Collect assistant text from rebuilt output items (chat shim).
#[must_use]
pub fn output_text(items: &[Value]) -> String {
    let mut text = String::new();
    for item in items {
        append_item_text(&mut text, item);
    }
    text
}

fn append_item_text(text: &mut String, item: &Value) {
    if item.get("type").and_then(Value::as_str) == Some("function_call") {
        return;
    }
    match item.get("content") {
        Some(Value::Array(parts)) => {
            for part in parts {
                if let Some(chunk) = part.get("text").and_then(Value::as_str) {
                    text.push_str(chunk);
                }
            }
        }
        Some(Value::String(chunk)) => text.push_str(chunk),
        _ => {
            if let Some(chunk) = item.get("text").and_then(Value::as_str) {
                text.push_str(chunk);
            }
        }
    }
}
