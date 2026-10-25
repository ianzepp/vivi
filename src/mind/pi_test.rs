//! Tests for [`crate::mind::pi`]: the record codec and the fake. The
//! subprocess adapter is not exercised (pi is not installed here).

use serde_json::json;
use tokio::sync::mpsc;

use super::*;

#[test]
fn command_lines_are_one_lf_terminated_object() {
    let line = command_line("mind-1", "prompt", json!({ "message": "hi\nthere" }));
    assert!(line.ends_with('\n'));
    assert_eq!(line.matches('\n').count(), 1);
    let value: Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(value["id"], "mind-1");
    assert_eq!(value["type"], "prompt");
    assert_eq!(value["message"], "hi\nthere");
}

#[test]
fn records_parse() {
    let response = parse_record(
        r#"{"id":"mind-2","type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}"#,
    )
    .unwrap();
    let PiRecord::Response {
        id, success, data, ..
    } = response
    else {
        panic!("expected a response");
    };
    assert_eq!(id.as_deref(), Some("mind-2"));
    assert!(success);
    assert_eq!(data["disposition"], "started");
    assert_eq!(
        parse_record(
            r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"he"}}"#
        )
        .unwrap(),
        PiRecord::TextDelta("he".into())
    );
    assert_eq!(
        parse_record("{\"type\":\"agent_settled\"}\r").unwrap(),
        PiRecord::Settled
    );
    assert_eq!(
        parse_record(r#"{"type":"tool_execution_start"}"#).unwrap(),
        PiRecord::Other("tool_execution_start".into())
    );
    assert!(parse_record("not json").is_err());
}

#[test]
fn history_lines_render_the_tail() {
    let data = json!({ "messages": [
        { "role": "user", "content": "one" },
        { "role": "assistant", "content": [{ "type": "text", "text": "two" }] },
        { "role": "user", "content": "three\nlines" },
    ]});
    let lines = history_lines(&data, 2);
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("assistant: "));
    assert_eq!(lines[1], "user: three lines");
    assert!(history_lines(&json!({}), 5).is_empty());
}

#[tokio::test]
async fn fake_streams_scripted_replies_and_records_prompts() {
    let mut pi = FakePi::with_replies(&["first"]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let summary = pi.prompt("wake".into(), Some(tx)).await.unwrap();
    assert_eq!(summary.text, "first");
    assert_eq!(rx.recv().await, Some(TurnEvent::Text("first".into())));
    assert_eq!(rx.recv().await, Some(TurnEvent::Settled));
    assert_eq!(pi.prompt("again".into(), None).await.unwrap().text, "no-op");
    assert_eq!(pi.history(1).await.unwrap(), vec!["user: again"]);
    assert_eq!(pi.compact().await.unwrap(), "compacted 2 prompts");
}
