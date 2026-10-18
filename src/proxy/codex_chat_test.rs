use serde_json::json;

use super::codex_chat;
use super::sse::Event;

#[test]
fn drops_system_roles_and_forces_stream() {
    let body = json!({
        "model": "gpt-6.1-sol",
        "stream": false,
        "user": "someone",
        "input": [
            {"role": "system", "content": "nope"},
            {"role": "user", "content": "hi"}
        ]
    });
    let sanitized = codex_chat::sanitize_responses(body);
    assert_eq!(sanitized["stream"], true);
    assert!(sanitized.get("user").is_none());
    assert_eq!(sanitized["input"].as_array().unwrap().len(), 1);
}

#[test]
fn chat_shim_drops_tools_and_reports_zero_usage() {
    let chat = json!({
        "model": "gpt-6.1-sol",
        "messages": [
            {"role": "system", "content": "nope"},
            {"role": "user", "content": "hi"}
        ],
        "tools": [{"type": "function", "function": {"name": "x"}}]
    });
    let responses = codex_chat::chat_to_responses(&chat);
    assert!(responses.get("tools").is_none());
    assert_eq!(responses["input"].as_array().unwrap().len(), 1);
    let out = codex_chat::responses_to_chat("gpt-6.1-sol", "hello", "abc");
    assert_eq!(out["usage"]["total_tokens"], 0);
    assert_eq!(out["choices"][0]["message"]["content"], "hello");
}

#[test]
fn collects_text_from_sse_deltas() {
    let events = [Event {
        raw: "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n".into(),
    }];
    assert_eq!(codex_chat::text_from_events(&events), "hi");
}
