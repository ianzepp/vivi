//! Coverage for the fake project server's test controls, driven over
//! loopback HTTP: the one-shot key-mint response barrier (pause after the
//! key exists but before its response lands, concurrent progress while
//! parked, release, bounded waits, teardown safety) and the revocation
//! refusal with its attempt recorder.

#[path = "support/project_server.rs"]
mod project_server;

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vivi::b64;
use vivi::sync::identity::SyncIdentity;

use project_server::{HttpReply, ProjectServer, http};

/// Generous bound for waits that should conclude quickly, even on a slow
/// host; the client is parked by the test's own hand, never by load.
const SETTLE: Duration = Duration::from_secs(5);
/// A deadline short enough to prove a bounded wait gives up promptly.
const NUDGE: Duration = Duration::from_millis(150);

/// A fresh identity; the tempdir can go, the key stays live in memory.
fn identity() -> SyncIdentity {
    let dir = tempfile::tempdir().unwrap();
    SyncIdentity::ensure(&dir.path().join(".vivi")).unwrap()
}

fn error_code(reply: &HttpReply) -> String {
    let parsed: Value = serde_json::from_str(&reply.body).unwrap_or(Value::Null);
    parsed["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn call(
    server: &ProjectServer,
    method: &str,
    tail: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> HttpReply {
    let path = format!("/v1/projects/{}/{}", server.project(), tail);
    let mut headers: Vec<(String, String)> = Vec::new();
    if body.is_some() {
        headers.push(("Content-Type".into(), "application/json".into()));
    }
    if let Some(bearer) = bearer {
        headers.push(("Authorization".into(), format!("Bearer {bearer}")));
    }
    let body_text = body.map(|value| value.to_string());
    let header_refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    http(
        &server.url(),
        method,
        &path,
        &header_refs,
        body_text.as_deref(),
    )
}

fn post(server: &ProjectServer, tail: &str, bearer: Option<&str>, body: Value) -> HttpReply {
    call(server, "POST", tail, bearer, Some(body))
}

fn get(server: &ProjectServer, tail: &str, bearer: Option<&str>) -> HttpReply {
    call(server, "GET", tail, bearer, None)
}

fn delete(server: &ProjectServer, tail: &str, bearer: Option<&str>) -> HttpReply {
    call(server, "DELETE", tail, bearer, None)
}

/// The full dance: challenge, sign the 32 challenge bytes, mint a token.
fn mint_token(server: &ProjectServer, identity: &SyncIdentity, name: &str) -> String {
    let challenge = post(
        server,
        "identities/challenge",
        None,
        json!({ "public_key_pem": identity.public_key_pem() }),
    );
    assert_eq!(
        challenge.status, 200,
        "challenge status {}",
        challenge.status
    );
    let parsed: Value = serde_json::from_str(&challenge.body).unwrap();
    let bytes = b64::decode(parsed["challenge"].as_str().unwrap()).unwrap();
    let minted = post(
        server,
        "auth/token",
        None,
        json!({
            "name": name,
            "challenge_id": parsed["challenge_id"].as_str().unwrap(),
            "signature_base64": b64::encode(&identity.sign(&bytes)),
        }),
    );
    assert_eq!(minted.status, 200, "token status {}", minted.status);
    let parsed: Value = serde_json::from_str(&minted.body).unwrap();
    parsed["token"].as_str().unwrap().to_string()
}

/// `POST /keys` on a background thread, so the test can hold the barrier
/// while the dispatching client is still waiting for its response.
fn mint_key_async(url: &str, project: &str, token: &str, label: &str) -> mpsc::Receiver<HttpReply> {
    let (done, minted) = mpsc::channel();
    let auth = format!("Bearer {token}");
    let body = json!({ "label": label }).to_string();
    let path = format!("/v1/projects/{project}/keys");
    let url = url.to_string();
    thread::spawn(move || {
        let reply = http(
            &url,
            "POST",
            &path,
            &[
                ("Content-Type", "application/json"),
                ("Authorization", auth.as_str()),
            ],
            Some(&body),
        );
        done.send(reply).ok();
    });
    minted
}

#[test]
fn mint_pauses_after_create_until_release_and_others_progress() {
    let server = ProjectServer::start();
    let identity = identity();
    server.register("alice", &identity.public_key_pem());
    let token = mint_token(&server, &identity, "alice");

    server.arm_key_barrier();
    let minted = mint_key_async(&server.url(), server.project(), &token, "handoff");

    // Creation is observable while the dispatching client is still parked.
    assert!(
        server.wait_key_barrier(SETTLE),
        "no key was created behind the barrier"
    );
    let keys = server.keys();
    assert_eq!(keys.len(), 1, "the key exists before its response lands");
    assert_eq!(keys[0].label, "handoff");
    let key_id = keys[0].key_id.clone();

    // List, auth, and revoke requests keep progressing while the mint
    // waits: the barrier never holds State's mutex.
    let listed = get(&server, "keys", Some(&token));
    assert_eq!(listed.status, 200, "list while parked");
    assert!(listed.body.contains("handoff"), "the created key is listed");
    let challenge = post(
        &server,
        "identities/challenge",
        None,
        json!({ "public_key_pem": identity.public_key_pem() }),
    );
    assert_eq!(challenge.status, 200, "auth while parked");
    let revoked = delete(&server, &format!("keys/{key_id}"), Some(&token));
    assert_eq!(revoked.status, 204, "revoke while parked");
    assert!(
        server.key_barrier_parked(),
        "still parked after the side traffic"
    );

    server.release_key_barrier();
    let reply = minted
        .recv_timeout(SETTLE)
        .expect("the parked mint finished after release");
    assert_eq!(reply.status, 201, "the released response still succeeds");
    let value: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["key_id"], key_id.as_str());
    assert!(
        !server.key_barrier_parked(),
        "the barrier handed its response back"
    );
}

#[test]
fn barrier_wait_gives_up_at_its_deadline_without_a_mint() {
    let server = ProjectServer::start();
    let started = Instant::now();
    assert!(
        !server.wait_key_barrier(NUDGE),
        "nothing minted, so no creation"
    );
    assert!(
        started.elapsed() < SETTLE,
        "the bounded wait returned on its deadline, not by hanging"
    );
    assert!(!server.key_barrier_parked());
    assert_eq!(server.keys().len(), 0, "no key was minted");
}

#[test]
fn dropping_the_server_hands_back_a_parked_mint() {
    let server = ProjectServer::start();
    let identity = identity();
    server.register("alice", &identity.public_key_pem());
    let token = mint_token(&server, &identity, "alice");

    server.arm_key_barrier();
    let minted = mint_key_async(&server.url(), server.project(), &token, "handoff");
    assert!(server.wait_key_barrier(SETTLE));
    assert_eq!(server.keys().len(), 1, "created before the drop");

    drop(server);
    let reply = minted
        .recv_timeout(SETTLE)
        .expect("teardown freed the parked mint thread");
    assert_eq!(
        reply.status, 201,
        "the parked response is handed back on drop"
    );
}

#[test]
fn refused_revocation_is_recorded_and_leaves_the_key_live() {
    let server = ProjectServer::start();
    let identity = identity();
    server.register("alice", &identity.public_key_pem());
    let token = mint_token(&server, &identity, "alice");
    server.issue_api_key("alice");
    let key_id = server.keys()[0].key_id.clone();
    let label = server.keys()[0].label.clone();

    server.refuse_revocations();
    let refused = delete(&server, &format!("keys/{key_id}"), Some(&token));
    assert_eq!(
        refused.status, 409,
        "the refusal control rejected the revoke"
    );
    assert_eq!(error_code(&refused), "revoke_refused");

    let attempts = server.revoke_attempts();
    assert_eq!(attempts.len(), 1, "the failed attempt is on the record");
    assert_eq!(attempts[0].key_id, key_id);
    assert_eq!(attempts[0].label.as_deref(), Some(label.as_str()));
    assert_eq!(attempts[0].member, "alice");
    assert!(attempts[0].refused, "the refusal is flagged on the attempt");

    let keys = server.keys();
    assert!(!keys[0].revoked, "a refused revoke leaves the key live");
    let listed = get(&server, "keys", Some(&token));
    assert_eq!(listed.status, 200);
    assert!(
        !listed.body.contains("revoked_at"),
        "the list still reports the refused key as live"
    );
}

#[test]
fn revoke_attempts_are_recorded_separate_from_the_refusal_control() {
    let server = ProjectServer::start();
    let identity = identity();
    server.register("alice", &identity.public_key_pem());
    let token = mint_token(&server, &identity, "alice");
    server.issue_api_key("alice");
    let key_id = server.keys()[0].key_id.clone();

    // A successful revoke is a plain recorded attempt, not a refusal.
    let done = delete(&server, &format!("keys/{key_id}"), Some(&token));
    assert_eq!(done.status, 204);
    // An unauthenticated revoke fails on auth alone and is still recorded.
    let anonymous = delete(&server, &format!("keys/{key_id}"), None);
    assert_eq!(anonymous.status, 401);

    let attempts = server.revoke_attempts();
    assert_eq!(attempts.len(), 2, "both attempts are on the record");
    assert_eq!(attempts[0].member, "alice");
    assert!(
        !attempts[0].refused,
        "a 204 without the control is no refusal"
    );
    assert_eq!(attempts[1].member, "", "no member behind a missing token");
    assert!(!attempts[1].refused, "a 401 is not the refusal control");
}
