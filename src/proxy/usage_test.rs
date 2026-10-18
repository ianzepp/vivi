use super::usage;

#[test]
fn harvests_response_completed_usage() {
    let data = r#"{"type":"response.completed","response":{"id":"resp_1","usage":{"input_tokens":3,"output_tokens":1}}}"#;
    let usage = usage::from_sse_data(data).expect("completed usage");
    assert_eq!(usage.input, Some(3));
    assert_eq!(usage.output, Some(1));
    assert_eq!(usage.upstream_id.as_deref(), Some("resp_1"));
}

#[test]
fn harvests_chat_usage_chunk() {
    let data = r#"{"id":"chatcmpl-1","usage":{"prompt_tokens":4,"completion_tokens":2}}"#;
    let usage = usage::from_sse_data(data).expect("chat usage");
    assert_eq!(usage.input, Some(4));
    assert_eq!(usage.output, Some(2));
}
