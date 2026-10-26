//! `tools/list` registration of the operator-only mind reads on `/v1/mcp`:
//! the `need_show`, `need_thread`, `task_thread` and `pump_priority`
//! schemas — types, bounds and required arguments — and the required
//! `project` argument the server endpoint injects into every project tool.

#[path = "support/mind_server.rs"]
mod mind_server;

use serde_json::{Value, json};

use vivi::server::safety::RateLimiter;

/// The registered tool entry named `name` on an operator key's tools/list.
async fn entry(fx: &mind_server::Fx, key: &str, name: &str) -> Value {
    let tools = fx.tools(key).await;
    tools
        .into_iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("{name} is missing from tools/list"))
}

#[tokio::test]
async fn need_show_schema_takes_one_required_handle() {
    let fx = mind_server::fixture("mind-need-show-schema");
    let key = fx.key_by(&fx.member, Some("operator"));
    let schema = entry(&fx, &key, "need_show").await["inputSchema"].clone();
    assert_eq!(schema["type"], "object", "{schema}");
    assert_eq!(schema["additionalProperties"], false, "{schema}");
    assert_eq!(
        schema["properties"]["handle"],
        json!({"type": "string", "description": "The need handle to show"}),
        "{schema}"
    );
    assert_eq!(schema["required"], json!(["project", "handle"]), "{schema}");
}

#[tokio::test]
async fn need_thread_schema_bounds_its_page() {
    let fx = mind_server::fixture("mind-need-thread-schema");
    let key = fx.key_by(&fx.member, Some("operator"));
    let schema = entry(&fx, &key, "need_thread").await["inputSchema"].clone();
    assert_eq!(schema["type"], "object", "{schema}");
    assert_eq!(schema["additionalProperties"], false, "{schema}");
    assert_eq!(schema["properties"]["handle"]["type"], "string", "{schema}");
    assert_eq!(
        schema["properties"]["limit"],
        json!({
            "type": "integer",
            "minimum": 1,
            "maximum": 100,
            "default": 20,
            "description": "Letters returned"
        }),
        "{schema}"
    );
    assert_eq!(schema["properties"]["before"]["type"], "string", "{schema}");
    assert_eq!(schema["required"], json!(["project", "handle"]), "{schema}");
}

#[tokio::test]
async fn task_thread_schema_bounds_its_page() {
    let fx = mind_server::fixture("mind-task-thread-schema");
    let key = fx.key_by(&fx.member, Some("operator"));
    let schema = entry(&fx, &key, "task_thread").await["inputSchema"].clone();
    assert_eq!(schema["type"], "object", "{schema}");
    assert_eq!(schema["additionalProperties"], false, "{schema}");
    assert_eq!(schema["properties"]["handle"]["type"], "string", "{schema}");
    assert_eq!(
        schema["properties"]["limit"],
        json!({
            "type": "integer",
            "minimum": 1,
            "maximum": 100,
            "default": 20,
            "description": "Letters returned"
        }),
        "{schema}"
    );
    assert_eq!(schema["properties"]["before"]["type"], "string", "{schema}");
    assert_eq!(schema["required"], json!(["project", "handle"]), "{schema}");
}

#[tokio::test]
async fn pump_priority_schema_keeps_action_need_level() {
    let fx = mind_server::fixture("mind-pump-priority-schema");
    let key = fx.key_by(&fx.member, Some("operator"));
    let schema = entry(&fx, &key, "pump_priority").await["inputSchema"].clone();
    assert_eq!(schema["type"], "object", "{schema}");
    assert_eq!(schema["additionalProperties"], false, "{schema}");
    assert_eq!(
        schema["properties"]["action"]["enum"],
        json!(["set", "clear", "list"]),
        "{schema}"
    );
    assert_eq!(schema["properties"]["need"]["type"], "string", "{schema}");
    assert_eq!(
        schema["properties"]["level"],
        json!({
            "type": "integer",
            "minimum": 0,
            "maximum": i64::MAX,
            "description": "The priority level; required for set, refused beside clear and list"
        }),
        "{schema}"
    );
    assert_eq!(schema["required"], json!(["project", "action"]), "{schema}");
}

/// The server endpoint injects a required `project` argument into every
/// project tool and keeps the tool's own required arguments.
#[tokio::test]
async fn project_argument_is_injected_into_the_mind_schemas() {
    let fx = mind_server::fixture("mind-project-injection");
    let key = fx.key_by(&fx.member, Some("operator"));
    for (name, own) in [
        ("need_show", "handle"),
        ("need_thread", "handle"),
        ("task_thread", "handle"),
        ("pump_priority", "action"),
    ] {
        let schema = entry(&fx, &key, name).await["inputSchema"].clone();
        assert_eq!(
            schema["properties"]["project"]["type"], "string",
            "{name}: {schema}"
        );
        let required = schema["required"]
            .as_array()
            .unwrap_or_else(|| panic!("{name}: required is not an array: {schema}"));
        assert!(required.contains(&json!("project")), "{name}: {schema}");
        assert!(required.contains(&json!(own)), "{name}: {schema}");
    }
}

/// `fixture_with` state customization selects the write limit the endpoint
/// counts priority writes against: the second write in the window is
/// refused while the read still answers.
#[tokio::test]
async fn fixture_write_limit_bounds_priority_writes() {
    let fx = mind_server::fixture_with("mind-write-limit", |state| {
        state.limiter = RateLimiter::new(Some(1));
    });
    let key = fx.key_by(&fx.member, Some("operator"));
    let answer = fx
        .call(
            &key,
            "pump_priority",
            json!({"action": "set", "need": "n1", "level": 2}),
        )
        .await;
    assert_eq!(
        answer,
        json!({"action": "set", "need": "n1", "level": 2}),
        "{answer}"
    );
    let message = fx
        .refused(
            &key,
            "pump_priority",
            json!({"action": "set", "need": "n2", "level": 3}),
        )
        .await;
    assert!(message.contains("writes/min"), "{message}");
    let listed = fx
        .call(&key, "pump_priority", json!({"action": "list"}))
        .await;
    assert_eq!(
        listed["priorities"],
        json!([{"need": "n1", "level": 2}]),
        "{listed}"
    );
}
