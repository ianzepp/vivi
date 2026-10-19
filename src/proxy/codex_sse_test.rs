use serde_json::json;

use super::codex_sse::{self, Rebuild};
use super::sse::Event;

fn event(value: &serde_json::Value) -> Event {
    Event {
        raw: format!("data: {value}\n\n"),
    }
}

#[test]
fn drops_unlisted_events() {
    let mut rebuild = Rebuild::default();
    let dropped = event(&json!({"type": "response.reasoning_summary_text.delta", "delta": "x"}));
    assert!(codex_sse::filter(dropped, &mut rebuild).is_none());
}

#[test]
fn rebuilds_empty_completed_output() {
    let mut rebuild = Rebuild::default();
    let item = json!({"id": "msg_1", "type": "message", "content": [{"text": "hi"}]});
    let done = event(&json!({"type": "response.output_item.done", "item": item}));
    assert!(codex_sse::filter(done, &mut rebuild).is_some());
    let completed = event(&json!({
        "type": "response.completed",
        "response": { "id": "resp_1", "output": [] }
    }));
    let mapped = codex_sse::filter(completed, &mut rebuild).expect("kept");
    let data: serde_json::Value = serde_json::from_str(&mapped.data().unwrap()).unwrap();
    assert_eq!(data["response"]["output"][0]["id"], "msg_1");
}

#[test]
fn rebuilt_frame_has_no_interior_blank_line() {
    let mut rebuild = Rebuild::default();
    rebuild.push_item(json!({"id": "msg_1", "type": "message", "content": [{"text": "hi"}]}));
    let completed = Event {
        raw: "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n"
            .to_string(),
    };
    let mapped = codex_sse::filter(completed, &mut rebuild).expect("kept");
    let trimmed = mapped.raw.trim_end_matches('\n');
    assert!(
        !trimmed.contains("\n\n"),
        "interior blank line in {:?}",
        mapped.raw
    );
    assert!(
        trimmed.starts_with("event: response.completed\ndata: "),
        "bad frame: {:?}",
        mapped.raw
    );
}

#[test]
fn rebuilt_frame_round_trips() {
    let mut rebuild = Rebuild::default();
    rebuild.push_item(json!({"id": "msg_1", "type": "message", "content": [{"text": "hi"}]}));
    let completed = Event {
        raw: "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n"
            .to_string(),
    };
    let mapped = codex_sse::filter(completed, &mut rebuild).expect("kept");
    let mut buffer = mapped.to_bytes().to_vec();
    let events = super::sse::take_events(&mut buffer);
    assert_eq!(events.len(), 1, "expected one frame, got {events:?}");
    let data: serde_json::Value = serde_json::from_str(&events[0].data().unwrap()).unwrap();
    assert_eq!(data["type"], "response.completed");
}

#[test]
fn keeps_populated_completed_output() {
    let mut rebuild = Rebuild::default();
    rebuild.push_item(json!({"id": "ignored"}));
    let completed = event(&json!({
        "type": "response.completed",
        "response": { "output": [{"id": "kept"}] }
    }));
    let mapped = codex_sse::filter(completed, &mut rebuild).expect("kept");
    let data: serde_json::Value = serde_json::from_str(&mapped.data().unwrap()).unwrap();
    assert_eq!(data["response"]["output"][0]["id"], "kept");
}
