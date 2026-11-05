use super::*;

#[tokio::test]
async fn mcp_unaddressed_send_uses_grant_identity() {
    let fx = fixture("unaddressed-authorship");
    let token = fx.key(Some("hand"));
    let listed = fx.rpc(&token, "tools/list", json!({})).await;
    let send = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "send")
        .unwrap();
    assert!(
        send["description"]
            .as_str()
            .unwrap()
            .contains("author-only")
    );
    let sent = fx
        .call(
            &token,
            "send",
            json!({
                "kind": "mail", "subject": "solo", "body": "private", "to": []
            }),
        )
        .await;
    assert_eq!(sent["from"], "hand");
    assert!(sent["delivered"].as_array().unwrap().is_empty());
    assert!(sent["handle"].as_str().is_some());
}

#[tokio::test]
async fn mcp_from_cannot_change_the_grant_author() {
    let fx = fixture("from-assertion");
    let token = fx.key(Some("hand"));
    let error = fx
        .call_err(
            &token,
            "send",
            json!({
                "kind": "mail", "subject": "bad", "body": "must not write", "from": "mind"
            }),
        )
        .await;
    assert!(error.contains("assert"), "{error}");
    let sent = fx
        .call(
            &token,
            "send",
            json!({
                "kind": "mail", "subject": "good", "body": "stored", "from": "hand", "to": []
            }),
        )
        .await;
    assert_eq!(sent["from"], "hand");
}
