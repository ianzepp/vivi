use super::*;

#[test]
fn status_is_a_reply_on_the_need() {
    let call = RecordCall::Status {
        need: "39dab91b".into(),
        status: "building".into(),
    };
    let calls = mcp_calls(&call, "faberlang", "");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "reply");
    assert_eq!(calls[0].1["handle"], "39dab91b");
    assert_eq!(calls[0].1["body"], "status: building");
    assert!(calls[0].1.get("for").is_none());
}

#[test]
fn park_marks_with_the_class_and_replies_with_the_cause() {
    let call = RecordCall::Park {
        need: "n1".into(),
        slice: "s2".into(),
        cause: ParkCause::InfraExhausted {
            step: "implement".into(),
            detail: "host lost".into(),
        },
    };
    let calls = mcp_calls(&call, "p", "operator");
    let tools: Vec<&str> = calls.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(tools, vec!["mark", "reply"]);
    assert_eq!(calls[0].1["marks"], json!(["parked", "parked-infra"]));
    assert_eq!(calls[0].1["for"], "operator");
    let body = calls[1].1["body"].as_str().unwrap();
    assert!(
        body.contains("slice s2") && body.contains("host lost"),
        "{body}"
    );
}

#[test]
fn close_is_done_with_the_summary() {
    let call = RecordCall::Close {
        need: "n1".into(),
        summary: "landed s1".into(),
    };
    let calls = mcp_calls(&call, "p", "");
    assert_eq!(calls[0].0, "done");
    assert_eq!(calls[0].1["note"], "landed s1");
}

#[test]
fn memory_record_keeps_calls_per_need() {
    let record = MemoryRecord::default();
    let a = RecordCall::Close {
        need: "a".into(),
        summary: String::new(),
    };
    record.apply(&a).unwrap();
    assert_eq!(record.calls_for("a").unwrap(), vec![a]);
    assert!(record.calls_for("b").unwrap().is_empty());
}

#[test]
fn log_handles_skip_the_server() {
    // An unreachable server and a missing key: a stress handle must not
    // touch either.
    let record = McpRecord {
        config: crate::config::RecordConfig {
            kind: crate::config::RecordKind::Mcp,
            server: "http://127.0.0.1:9".into(),
            project: "p".into(),
            key_file: "/nonexistent/key".into(),
            role: String::new(),
            vivi: "/nonexistent/vivi".into(),
            log_handles: vec!["stress-".into()],
        },
    };
    let call = RecordCall::Status {
        need: "stress-r1-01".into(),
        status: "building".into(),
    };
    assert!(record.apply(&call).is_ok());
    let real = RecordCall::Status {
        need: "8a2cd8ba".into(),
        status: "building".into(),
    };
    assert!(
        record.apply(&real).is_err(),
        "a real handle still goes to the server"
    );
}
