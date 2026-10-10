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
    let (status, project) = send(&app, "POST", "/v1/projects", None, None).await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    let project_id = project["project_id"].as_str().unwrap();

    let (status, postmaster) = register(&app, project_id, &key(1), "postmaster").await;
    assert_eq!(status, StatusCode::CONFLICT, "{postmaster}");
    let (status, identity) = register(&app, project_id, &key(2), "builder").await;
    assert_eq!(status, StatusCode::CREATED, "{identity}");
    let token = identity["token"].as_str().unwrap();
    let (status, duplicate) = register(&app, project_id, &key(3), "builder").await;
    assert_eq!(status, StatusCode::CONFLICT, "{duplicate}");

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
    assert_eq!(items["next_cursor"], 2);
    assert_eq!(items["records"][0]["sequence"], 1);
    assert_eq!(items["records"][0]["body"], "body 0");
    assert_eq!(items["records"][1]["sequence"], 2);
    assert_eq!(items["records"][1]["body"], "body 1");
}

#[tokio::test]
async fn project_directory_lists_authenticated_members_sorted_by_name() {
    let dir = data_dir();
    let app = api_router(dir, Some(STANDARD.encode([0x42u8; 32])), None);
    let (status, project) = send(&app, "POST", "/v1/projects", None, None).await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    let project_id = project["project_id"].as_str().unwrap();

    let (status, zulu) = register(&app, project_id, &key(20), "zulu").await;
    assert_eq!(status, StatusCode::CREATED, "{zulu}");
    let (status, alpha) = register(&app, project_id, &key(21), "alpha").await;
    assert_eq!(status, StatusCode::CREATED, "{alpha}");
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
