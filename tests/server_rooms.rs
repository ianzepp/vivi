//! Lightweight chat rooms (item 17): create, join, send, read, close, expire.

use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;

use vivi_server::api_router;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vivi-server-chat-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    if bytes.is_empty() {
        return (status, Value::Null);
    }
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned())),
    )
}

fn app(name: &str) -> (Router, PathBuf) {
    let dir = scratch(name);
    (api_router(dir.clone(), None, None), dir)
}

async fn create(app: &Router, name: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::builder()
            .method("POST")
            .uri("/v1/rooms")
            .header("content-type", "application/json")
            .body(Body::from(json!({ "name": name }).to_string()))
            .unwrap(),
    )
    .await
}

async fn join(app: &Router, room: &str, name: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/rooms/{room}/join"))
            .header("content-type", "application/json")
            .body(Body::from(json!({ "name": name }).to_string()))
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn create_join_send_read() {
    let (app, dir) = app("round-trip");
    let (status, created) = create(&app, "alice").await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let room = created["room_id"].as_str().unwrap().to_owned();
    assert_eq!(created["project_id"], room);
    assert_eq!(created["mode"], "chat");
    assert_eq!(created["name"], "alice");
    assert!(created["token"].as_str().is_some());
    assert_eq!(created["ttl_seconds"], 86400);
    assert!(
        dir.join("projects")
            .join(&room)
            .join("project.json")
            .is_file(),
        "chat lives under projects/, not rooms/"
    );
    assert!(
        !dir.join("rooms").join(&room).exists(),
        "must not create a second tenant under rooms/"
    );

    let alice = created["token"].as_str().unwrap().to_owned();
    let (status, joined) = join(&app, &room, "bob").await;
    assert_eq!(status, StatusCode::CREATED, "{joined}");
    let bob = joined["token"].as_str().unwrap().to_owned();

    let (status, sent) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/rooms/{room}/messages"))
            .header("authorization", format!("Bearer {alice}"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "body": "hello from alice" }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{sent}");
    assert_eq!(sent["kind"], "chat");
    assert!(sent["handle"].as_str().is_some());

    let (status, page) = send(
        &app,
        Request::builder()
            .uri(format!("/v1/rooms/{room}/messages?since=0"))
            .header("authorization", format!("Bearer {bob}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);
    assert_eq!(page["messages"][0]["from"], "alice");
    assert_eq!(page["messages"][0]["body"], "hello from alice");
}

#[tokio::test]
async fn join_requires_the_room_id_and_rejects_taken_names() {
    let (app, _) = app("join-rules");
    let (status, created) = create(&app, "alice").await;
    assert_eq!(status, StatusCode::CREATED);
    let room = created["room_id"].as_str().unwrap().to_owned();

    let (status, body) = join(&app, &room, "alice").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "name_taken");

    let (status, body) = join(&app, &room, "postmaster").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let unknown = "22222-22222-22222-22222-22222";
    let (status, body) = join(&app, unknown, "carol").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "room_not_found");

    let (status, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/v1/rooms")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "name": "x", "ttl_seconds": 10 }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn messages_need_a_bearer_and_are_member_visible() {
    let (app, _) = app("auth-vis");
    let (_, created) = create(&app, "alice").await;
    let room = created["room_id"].as_str().unwrap().to_owned();
    let alice = created["token"].as_str().unwrap().to_owned();
    let (_, joined) = join(&app, &room, "bob").await;
    let bob = joined["token"].as_str().unwrap().to_owned();

    let (status, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/rooms/{room}/messages"))
            .header("content-type", "application/json")
            .body(Body::from(json!({ "body": "nope" }).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let _ = send(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/rooms/{room}/messages"))
            .header("authorization", format!("Bearer {alice}"))
            .header("content-type", "application/json")
            .body(Body::from(json!({ "body": "visible to bob" }).to_string()))
            .unwrap(),
    )
    .await;

    let (status, page) = send(
        &app,
        Request::builder()
            .uri(format!("/v1/rooms/{room}/messages"))
            .header("authorization", format!("Bearer {bob}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["messages"][0]["body"], "visible to bob");
}

#[tokio::test]
async fn close_blocks_join_and_send() {
    let (app, _) = app("close");
    let (_, created) = create(&app, "alice").await;
    let room = created["room_id"].as_str().unwrap().to_owned();
    let token = created["token"].as_str().unwrap().to_owned();

    let (status, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/rooms/{room}/close"))
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["closed"], true);

    let (status, body) = join(&app, &room, "bob").await;
    assert_eq!(status, StatusCode::GONE, "{body}");
    assert_eq!(body["error"]["code"], "room_closed");

    let (status, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/rooms/{room}/messages"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(json!({ "body": "too late" }).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "{body}");
}

#[tokio::test]
async fn expire_blocks_writes_and_health_sweeps() {
    let dir = scratch("expire");
    let app = api_router(dir.clone(), None, Some("mod".into()));
    let (_, created) = create(&app, "alice").await;
    let room = created["room_id"].as_str().unwrap().to_owned();
    let token = created["token"].as_str().unwrap().to_owned();

    let path = dir.join("projects").join(&room).join("project.json");
    let mut meta: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    meta["expires_at"] = json!("2020-01-01T00:00:00Z");
    std::fs::write(&path, serde_json::to_vec(&meta).unwrap()).unwrap();

    let (status, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/rooms/{room}/messages"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(json!({ "body": "too late" }).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "{body}");
    assert_eq!(body["error"]["code"], "room_expired");

    let (status, health) = send(
        &app,
        Request::builder()
            .uri("/v1/_health")
            .header("authorization", "Bearer mod")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{health}");
    assert_eq!(health["rooms"], 0);
    assert!(
        !dir.join("projects").join(&room).exists(),
        "expired chat is swept on health"
    );
}

#[tokio::test]
async fn mcp_alias_sends_chat() {
    let (app, _) = app("mcp");
    let (_, created) = create(&app, "alice").await;
    let room = created["room_id"].as_str().unwrap().to_owned();
    let token = created["token"].as_str().unwrap().to_owned();

    let rpc = |uri: String, body: Value| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };

    let (status, init) = send(
        &app,
        rpc(
            format!("/v1/rooms/{room}/mcp"),
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-03-26","capabilities":{},
                "clientInfo":{"name":"t","version":"0"}
            }}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{init}");
    assert!(
        init["result"]["serverInfo"]["name"]
            .as_str()
            .unwrap()
            .contains("vivi-server")
    );

    let (status, sent) = send(
        &app,
        rpc(
            format!("/v1/rooms/{room}/mcp"),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                "name":"send","arguments":{
                    "kind":"chat","subject":"hi","body":"from mcp"
                }
            }}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    let text = sent["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("\"handle\""), "{text}");

    let (status, gone) = send(
        &app,
        rpc(
            format!("/v1/projects/{room}/mcp"),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
                "name":"list","arguments":{"kind":"chat","status":"all"}
            }}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{gone}");

    let (status, listed) = send(
        &app,
        rpc(
            format!("/v1/rooms/{room}/mcp"),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                "name":"list","arguments":{"kind":"chat","status":"all"}
            }}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let text = listed["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("from mcp") || text.contains("hi"), "{text}");
}

#[tokio::test]
async fn guest_cannot_mint_api_keys() {
    let (app, _) = app("no-keys");
    let (_, created) = create(&app, "alice").await;
    let room = created["room_id"].as_str().unwrap().to_owned();
    let token = created["token"].as_str().unwrap().to_owned();
    let (status, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/projects/{room}/keys"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(json!({ "label": "bot" }).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn metadata_distinguishes_bad_and_missing_ids() {
    let (app, _) = app("meta");
    let (_, created) = create(&app, "alice").await;
    let room = created["room_id"].as_str().unwrap().to_owned();

    let (status, body) = send(
        &app,
        Request::builder()
            .uri(format!("/v1/rooms/{room}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["members"], 1);
    assert_eq!(body["expired"], false);

    let (status, body) = send(
        &app,
        Request::builder()
            .uri("/v1/rooms/22222-22222-22222-22222-22222")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "room_not_found");

    let (status, body) = send(
        &app,
        Request::builder()
            .uri("/v1/rooms/not-a-crockford-id!")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_room_id");
}
