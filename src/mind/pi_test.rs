//! Tests for [`crate::mind::pi`]: the record codec and the fake. The
//! subprocess adapter is not exercised (pi is not installed here).

use serde_json::json;
use tokio::sync::mpsc;

use super::super::config::MindPaths;
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

/// A stand-in `pi --mode rpc` in sh: answers `get_state` with its own
/// `$HOME` (proving the cleared environment) and streams one prompt turn.
const SCRIPTED_PI: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"type":"get_state"'*)
      printf '{"type":"response","id":"%s","command":"get_state","success":true,"data":{"sessionId":"%s"}}\n' "$id" "$HOME" ;;
    *'"type":"prompt"'*)
      printf '{"type":"response","id":"%s","command":"prompt","success":true,"data":{}}\n' "$id"
      printf '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"all "}}\n'
      printf '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"quiet"}}\n'
      printf '{"type":"agent_settled"}\n' ;;
    *'"type":"compact"'*)
      printf '{"type":"response","id":"%s","command":"compact","success":false,"error":"nothing to compact"}\n' "$id" ;;
  esac
done
"#;

#[tokio::test]
async fn process_pi_speaks_jsonl_to_a_scripted_child() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("pi");
    std::fs::write(&script, SCRIPTED_PI).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = dir.path().join("home");
    let mut config =
        super::super::config::parse("", MindPaths::under(dir.path().join("mind.toml"), &home))
            .unwrap();
    config.pi_command = script.to_string_lossy().into_owned();

    let mut pi = ProcessPi::spawn(&config, None).unwrap();
    assert_eq!(
        pi.session_id().await.unwrap().as_deref(),
        Some(home.to_str().unwrap())
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    let summary = pi.prompt("anything stuck?".into(), Some(tx)).await.unwrap();
    assert_eq!(summary.text, "all quiet");
    assert_eq!(rx.recv().await, Some(TurnEvent::Text("all ".into())));
    let error = pi.compact().await.unwrap_err().to_string();
    assert!(error.contains("nothing to compact"), "{error}");
    assert!(!pi.exited());
}
