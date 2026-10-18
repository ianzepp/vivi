//! Chat Completions → Responses shim: non-streaming, no tools, usage 0.

use serde_json::{Value, json};

use super::codex_sse;

/// Fields dropped from a Responses body before it is sent upstream.
/// `store` is not dropped: the Codex backend requires `store=false`.
const DROP_FIELDS: &[&str] = &[
    "prompt_cache_key",
    "safety_identifier",
    "service_tier",
    "stream_options",
    "user",
    "metadata",
    "truncation",
    "include",
    "background",
];

/// Sanitize a Responses body: drop rejected fields, strip system/developer,
/// force `stream: true` and `store: false`.
#[must_use]
pub fn sanitize_responses(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        for field in DROP_FIELDS {
            object.remove(*field);
        }
        object.insert("stream".into(), json!(true));
        object.insert("store".into(), json!(false));
        if let Some(input) = object.get_mut("input") {
            *input = filter_input(input.take());
        }
    }
    value
}

fn filter_input(input: Value) -> Value {
    match input {
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .filter(|item| !is_system_or_developer(item))
                .collect(),
        ),
        other => other,
    }
}

fn is_system_or_developer(item: &Value) -> bool {
    matches!(
        item.get("role").and_then(Value::as_str),
        Some("system" | "developer")
    )
}

/// Convert a Chat Completions body into a sanitized Responses body.
/// Tools are dropped (the Python shim has no tools).
#[must_use]
pub fn chat_to_responses(chat: &Value) -> Value {
    let model = chat
        .get("model")
        .cloned()
        .unwrap_or_else(|| json!("gpt-6.1-sol"));
    let input = match chat.get("messages") {
        Some(Value::Array(messages)) => Value::Array(
            messages
                .iter()
                .filter(|item| !is_system_or_developer(item))
                .cloned()
                .collect(),
        ),
        _ => json!([]),
    };
    sanitize_responses(json!({
        "model": model,
        "input": input,
        "stream": true,
    }))
}

/// Build the non-streaming Chat Completions JSON the Python shim returns.
/// Usage is always zero.
#[must_use]
pub fn responses_to_chat(model: &str, text: &str, id: &str) -> Value {
    json!({
        "id": format!("chatcmpl-{id}"),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        }
    })
}

/// Collect assistant text from a finished Responses stream (events already filtered).
#[must_use]
pub fn text_from_events(events: &[super::sse::Event]) -> String {
    let mut rebuild = super::codex_sse::Rebuild::default();
    let mut fallback = String::new();
    for event in events {
        if let Some(data) = event.data()
            && let Ok(value) = serde_json::from_str::<Value>(&data)
        {
            collect_delta(&mut fallback, &value);
            if value.get("type").and_then(Value::as_str) == Some("response.output_item.done")
                && let Some(item) = value.get("item").cloned()
            {
                rebuild.push_item(item);
            }
        }
    }
    let assembled = codex_sse::output_text(rebuild.items());
    if assembled.is_empty() {
        fallback
    } else {
        assembled
    }
}

fn collect_delta(out: &mut String, value: &Value) {
    if value.get("type").and_then(Value::as_str) == Some("response.output_text.delta")
        && let Some(delta) = value.get("delta").and_then(Value::as_str)
    {
        out.push_str(delta);
    }
}
