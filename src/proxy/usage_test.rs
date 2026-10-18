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

#[test]
fn harvests_float_and_string_token_counts() {
    let data = r#"{"id":"chatcmpl-2","usage":{"prompt_tokens":6.0,"completion_tokens":"3"}}"#;
    let usage = usage::from_sse_data(data).expect("coerced usage");
    assert_eq!(usage.input, Some(6));
    assert_eq!(usage.output, Some(3));
}

#[test]
fn harvests_llama_timings() {
    let data = r#"{"id":"cmpl-1","timings":{"prompt_n":9,"predicted_n":4}}"#;
    let usage = usage::from_sse_data(data).expect("timings usage");
    assert_eq!(usage.input, Some(9));
    assert_eq!(usage.output, Some(4));
}
