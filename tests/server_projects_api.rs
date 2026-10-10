use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi_server::api_router;
use vivi_server::crypto::StoreCipher;
use vivi_server::projects::{AppendItem, ProjectStore};

fn data_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vivi-server-project-api-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn spki_pem(key: &SigningKey) -> String {
    const PREFIX: &[u8] = &[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = PREFIX.to_vec();
    der.extend_from_slice(key.verifying_key().as_bytes());
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
        STANDARD.encode(der)
    )
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(
            builder
                .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// POST one project item with an optional Message-ID header (the shared
/// `send` helper cannot set it).
async fn post_item(
    app: &Router,
    uri: &str,
    body: Value,
    token: &str,
    message_id: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(id) = message_id {
        builder = builder.header("message-id", id);
    }
    let response = app
        .clone()
        .oneshot(
            builder
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn register(
    app: &Router,
    project: &str,
    signing_key: &SigningKey,
    name: &str,
) -> (StatusCode, Value) {
    let (status, challenge) = send(
        app,
        "POST",
        &format!("/v1/projects/{project}/identities/challenge"),
        Some(json!({"public_key_pem": spki_pem(signing_key)})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{challenge}");
    let bytes = STANDARD
        .decode(challenge["challenge"].as_str().unwrap())
        .unwrap();
    let signature = STANDARD.encode(signing_key.sign(&bytes).to_bytes());
    send(
        app,
        "POST",
        &format!("/v1/projects/{project}/identities"),
        Some(json!({
            "name": name,
            "public_key_pem": spki_pem(signing_key),
            "challenge_id": challenge["challenge_id"],
            "signature_base64": signature,
        })),
        None,
    )
    .await
}

#[tokio::test]
async fn project_identity_auth_and_items_wire() {
    let dir = data_dir();
    let app = api_router(dir.clone(), Some(STANDARD.encode([0x42u8; 32])), None);
    let creator_key = key(1);
    let (status, project) = send(
        &app,
        "POST",
        "/v1/projects",
        Some(json!({"public_key_pem": spki_pem(&creator_key), "creator_name": "founder"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    assert_eq!(project["admission"]["name"], "founder");
    assert_eq!(
        project["admission"]["fingerprint"].as_str().unwrap().len(),
        64
    );
    let project_id = project["project_id"].as_str().unwrap();

    let (status, creator) = register(&app, project_id, &creator_key, "founder").await;
    assert_eq!(status, StatusCode::CREATED, "{creator}");
    let (status, unadmitted) = register(&app, project_id, &key(2), "builder").await;
    assert_eq!(status, StatusCode::CONFLICT, "{unadmitted}");
    assert_eq!(unadmitted["error"]["code"], "admission_required");

    let cipher = StoreCipher::from_base64(&STANDARD.encode([0x42u8; 32])).unwrap();
    let store = ProjectStore::new(&dir);
    store
        .append(
            &cipher,
            project_id,
            &AppendItem {
                id: "msg_000000000000000000000099".into(),
                from: "founder".into(),
                to: Some("postmaster".into()),
                subject: "vouch".into(),
                body: json!({"vouch": {"name": "builder", "public_key_pem": spki_pem(&key(2))}})
                    .to_string(),
                kind: None,
                in_reply_to: None,
                refs: vec![],
            },
        )
        .unwrap();
    let (status, identity) = register(&app, project_id, &key(2), "builder").await;
    assert_eq!(status, StatusCode::CREATED, "{identity}");
    let token = identity["token"].as_str().unwrap();
    let (status, wrong_key) = register(&app, project_id, &key(3), "builder").await;
    assert_eq!(status, StatusCode::CONFLICT, "{wrong_key}");
    assert_eq!(wrong_key["error"]["code"], "admission_required");
    let (status, another_unadmitted) = register(&app, project_id, &key(4), "guest").await;
    assert_eq!(status, StatusCode::CONFLICT, "{another_unadmitted}");
    assert_eq!(another_unadmitted["error"]["code"], "admission_required");

    let cipher = StoreCipher::from_base64(&STANDARD.encode([0x42u8; 32])).unwrap();
    let store = ProjectStore::new(&dir);
    for (i, id) in [
        "msg_000000000000000000000001",
        "msg_000000000000000000000002",
    ]
    .iter()
    .enumerate()
    {
        store
            .append(
                &cipher,
                project_id,
                &AppendItem {
                    id: id.to_string(),
                    from: "builder".into(),
                    to: None,
                    subject: format!("item {i}"),
                    body: format!("body {i}"),
                    kind: None,
                    in_reply_to: None,
                    refs: vec![],
                },
            )
            .unwrap();
    }
    let (status, unauthorized) = send(
        &app,
        "GET",
        &format!("/v1/projects/{project_id}/items?since=0"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{unauthorized}");
    let (status, items) = send(
        &app,
        "GET",
        &format!("/v1/projects/{project_id}/items?since=0"),
        None,
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{items}");
    assert_eq!(items["next_cursor"], 3);
    assert_eq!(
        items["records"][0]["body"],
        json!({"vouch": {"name": "builder", "public_key_pem": spki_pem(&key(2))}}).to_string()
    );
    assert_eq!(items["records"][1]["sequence"], 2);
    assert_eq!(items["records"][1]["body"], "body 0");
    assert_eq!(items["records"][2]["sequence"], 3);
    assert_eq!(items["records"][2]["body"], "body 1");
}

#[tokio::test]
async fn project_directory_lists_authenticated_members_sorted_by_name() {
    let dir = data_dir();
    let app = api_router(dir.clone(), Some(STANDARD.encode([0x42u8; 32])), None);
    let alpha_key = key(21);
    let (status, project) = send(
        &app,
        "POST",
        "/v1/projects",
        Some(json!({"public_key_pem": spki_pem(&alpha_key), "creator_name": "alpha"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    let project_id = project["project_id"].as_str().unwrap();

    let (status, alpha) = register(&app, project_id, &alpha_key, "alpha").await;
    assert_eq!(status, StatusCode::CREATED, "{alpha}");
    let cipher = StoreCipher::from_base64(&STANDARD.encode([0x42u8; 32])).unwrap();
    ProjectStore::new(&dir)
        .append(
            &cipher,
            project_id,
            &AppendItem {
                id: "msg_000000000000000000000098".into(),
                from: "alpha".into(),
                to: Some("postmaster".into()),
                subject: "vouch".into(),
                body: json!({"vouch": {"name": "zulu", "public_key_pem": spki_pem(&key(20))}})
                    .to_string(),
                kind: None,
                in_reply_to: None,
                refs: vec![],
            },
        )
        .unwrap();
    let (status, zulu) = register(&app, project_id, &key(20), "zulu").await;
    assert_eq!(status, StatusCode::CREATED, "{zulu}");
    let token = alpha["token"].as_str().unwrap();

    let (status, missing_auth) = send(
        &app,
        "GET",
        &format!("/v1/projects/{project_id}/directory"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{missing_auth}");
    let (status, invalid_auth) = send(
        &app,
        "GET",
        &format!("/v1/projects/{project_id}/directory"),
        None,
        Some("not-a-token"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{invalid_auth}");

    let (status, directory) = send(
        &app,
        "GET",
        &format!("/v1/projects/{project_id}/directory"),
        None,
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{directory}");
    assert_eq!(directory["members"][0]["name"], "alpha");
    assert_eq!(directory["members"][0]["fingerprint"], alpha["fingerprint"]);
    assert_eq!(directory["members"][1]["name"], "zulu");
    assert_eq!(directory["members"][1]["fingerprint"], zulu["fingerprint"]);

    let unknown_project_id = format!(
        "{}{}",
        if project_id.starts_with('z') {
            "y"
        } else {
            "z"
        },
        &project_id[1..]
    );
    let (status, unknown_project) = send(
        &app,
        "GET",
        &format!("/v1/projects/{unknown_project_id}/directory"),
        None,
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{unknown_project}");
}

#[tokio::test]
async fn project_items_append_endpoint_wire() {
    let dir = data_dir();
    let app = api_router(dir.clone(), Some(STANDARD.encode([0x42u8; 32])), None);
    let creator_key = key(31);
    let (status, project) = send(
        &app,
        "POST",
        "/v1/projects",
        Some(json!({"public_key_pem": spki_pem(&creator_key), "creator_name": "creator"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    let project_id = project["project_id"].as_str().unwrap().to_owned();
    let (status, creator) = register(&app, &project_id, &creator_key, "creator").await;
    assert_eq!(status, StatusCode::CREATED, "{creator}");
    let token = creator["token"].as_str().unwrap().to_owned();
    let items_path = format!("/v1/projects/{project_id}/items");

    // No token is 401.
    let (status, no_token) =
        send(&app, "POST", &items_path, Some(json!({"body": "hi"})), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{no_token}");

    // A member posts a work item with a valid Message-ID: 201 plus sequence
    // (kind "TASK" exercises the lowercasing).
    let work_item = json!({"subject": "task one", "body": "do the thing", "kind": "TASK"});
    let (status, created) = post_item(
        &app,
        &items_path,
        work_item.clone(),
        &token,
        Some("msg_00000000000000000000a001"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["message_id"], "msg_00000000000000000000a001");
    assert_eq!(created["sequence"], 1);

    // Replaying the same id and bytes: 200 duplicate true, nothing new.
    let (status, duplicate) = post_item(
        &app,
        &items_path,
        work_item,
        &token,
        Some("msg_00000000000000000000a001"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{duplicate}");
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(duplicate["sequence"], 1);

    // No Message-ID header is 400.
    let (status, no_id) = post_item(&app, &items_path, json!({"body": "hi"}), &token, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{no_id}");
    assert_eq!(no_id["error"]["code"], "invalid_request");

    // A kinded item with a non-traveling id is 400.
    let (status, plain_id) = post_item(
        &app,
        &items_path,
        json!({"body": "hi", "kind": "task"}),
        &token,
        Some("plain-1"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{plain_id}");

    // A body over 64 KiB is 413.
    let (status, too_big) = post_item(
        &app,
        &items_path,
        json!({"body": "x".repeat(64 * 1024 + 1)}),
        &token,
        Some("msg_00000000000000000000a002"),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{too_big}");

    // A malformed vouch body is 400 with nothing appended (the valid vouch
    // below still lands at sequence 2).
    let (status, bad_vouch) = post_item(
        &app,
        &items_path,
        json!({"to": "postmaster", "body": "not a vouch"}),
        &token,
        Some("msg_00000000000000000000a003"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bad_vouch}");

    // A vouch to postmaster from the creator lands as an item.
    let builder_key = key(32);
    let (status, vouched) = post_item(
        &app,
        &items_path,
        json!({
            "to": "postmaster",
            "subject": "vouch",
            "body": json!({"vouch": {"name": "builder", "public_key_pem": spki_pem(&builder_key)}}).to_string()
        }),
        &token,
        Some("msg_00000000000000000000a004"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{vouched}");
    assert_eq!(vouched["sequence"], 2);

    // The vouched key can register afterward and the directory lists both.
    let (status, identity) = register(&app, &project_id, &builder_key, "builder").await;
    assert_eq!(status, StatusCode::CREATED, "{identity}");
    let (status, directory) = send(
        &app,
        "GET",
        &format!("/v1/projects/{project_id}/directory"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{directory}");
    let names: Vec<&str> = directory["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"creator"), "{directory}");
    assert!(names.contains(&"builder"), "{directory}");
}
