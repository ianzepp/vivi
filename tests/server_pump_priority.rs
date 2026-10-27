//! The `pump_priority` MCP tool on `/v1/mcp`: canonical operator priority
//! writes for a hosted project.
//!
//! The `set` and `clear` writes resolve the `need` anchor with the shared
//! locator — a full handle or an unambiguous prefix — refuse anything that
//! is not a need before anything is written, and store the row in the
//! configured pump home's `need_priority` table under the canonical handle:
//! the same key a dispatch send files its priority under. Level 0 removes
//! the row. The writes are live operator state: they answer while a running
//! pump holds the pump home's lock.
//!
//! Fixture: `support/mind_server.rs`, with a non-default `[pump]` home so
//! persisted state is compared against `priority::list` at the configured
//! home, never the default one.
//!
//! The authorization, rate and audit coverage here is the write-side mirror
//! of `tests/server_mind_reads.rs`: one audit line per authenticated write
//! call (`ok` or `refused`), never for a member bearer token's
//! pre-authentication refusal on the wire.

#[path = "support/mind_server.rs"]
mod mind_server;

use std::fs;
use std::path::PathBuf;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::RoleUpdate;
use vivi::pump::control::project_home;
use vivi::pump::priority;
use vivi::server::grants::{GrantLevel, GrantRecord};
use vivi::server::safety::RateLimiter;

/// The non-default pump home the fixture configures, relative to the
/// project root.
const PUMP_HOME: &str = "pump-home";

/// Point the fixture's `[pump]` table at the non-default [`PUMP_HOME`]; the
/// tools resolve the home from the config fresh on every call.
fn with_pump_home(fx: &mind_server::Fx) {
    let toml = fx.root.join(".vivi").join("mailspace.toml");
    let mut text = std::fs::read_to_string(&toml).unwrap();
    text.push_str(format!("home = \"{PUMP_HOME}\"\n").as_str());
    std::fs::write(&toml, text).unwrap();
}

/// The pump home the tool writes to, resolved the way the tool resolves it.
fn home(fx: &mind_server::Fx) -> PathBuf {
    project_home(&fx.root).unwrap()
}

/// The stored (need, level) rows at the fixture's configured home.
fn stored(fx: &mind_server::Fx) -> Vec<(String, i64)> {
    priority::list(&home(fx)).unwrap()
}

/// One operator key and one seeded need: the anchor every write names.
fn operator_and_need(fx: &mind_server::Fx) -> (String, String) {
    let key = fx.key_by(&fx.member, Some("operator"));
    let need = fx.send(
        "mind",
        "need",
        "hand",
        "fix the flux capacitor",
        "+++\ndone_when = [\"it is fixed\"]\n+++\n\nplease fix it",
    );
    (key, need)
}

/// The pump home's lock file, held the way a running pump holds it.
fn hold_pump_lock(fx: &mind_server::Fx) -> std::fs::File {
    let home = home(fx);
    std::fs::create_dir_all(&home).unwrap();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("lock"))
        .unwrap();
    lock.try_lock().unwrap();
    lock
}

/// `set` requires `need` and a whole non-negative `level`: missing, string,
/// fractional, negative, boolean and null levels are refused, as are an
/// unknown action, a level beside `clear`, and need or level beside `list`.
/// Nothing is stored by any refusal.
#[tokio::test]
async fn set_refuses_bad_argument_types_and_actions_without_writing() {
    let fx = mind_server::fixture("priority-argument-types");
    with_pump_home(&fx);
    let (key, need) = operator_and_need(&fx);
    let bad = [
        json!({"action": "set", "level": 2}),
        json!({"action": "set", "need": need, "level": "2"}),
        json!({"action": "set", "need": need, "level": 1.5}),
        json!({"action": "set", "need": need, "level": -1}),
        json!({"action": "set", "need": need, "level": true}),
        json!({"action": "set", "need": need, "level": null}),
        json!({"action": "set", "need": need}),
        json!({"action": "wipe", "need": need, "level": 1}),
        json!({"action": "clear", "need": need, "level": 1}),
        json!({"action": "list", "need": need}),
        json!({"action": "list", "level": 1}),
    ];
    for arguments in &bad {
        let message = fx.refused(&key, "pump_priority", arguments.clone()).await;
        assert!(
            !message.is_empty(),
            "{arguments}: refused without a message"
        );
    }
    assert!(stored(&fx).is_empty(), "{:?}", stored(&fx));
}

/// The anchor must exist and be a need: an unknown handle, a task and a
/// plain letter are all refused before anything is written.
#[tokio::test]
async fn set_refuses_unknown_and_non_need_anchors_without_writing() {
    let fx = mind_server::fixture("priority-anchor-refusals");
    with_pump_home(&fx);
    let (key, _) = operator_and_need(&fx);
    let task = fx.send("mind", "task", "hand", "unit task", "+++\n+++\n\ndo it");
    let mail = fx.send("mind", "mail", "hand", "plain letter", "hello");
    for arguments in [
        json!({"action": "set", "need": "no-such-handle", "level": 1}),
        json!({"action": "set", "need": task, "level": 1}),
        json!({"action": "set", "need": mail, "level": 1}),
        json!({"action": "clear", "need": "no-such-handle"}),
    ] {
        let message = fx.refused(&key, "pump_priority", arguments).await;
        assert!(!message.is_empty(), "{message}");
    }
    assert!(stored(&fx).is_empty(), "{:?}", stored(&fx));
}

/// A prefix resolves to the canonical need: the response names the full
/// handle and the row is stored under it — the key a dispatch send files.
#[tokio::test]
async fn set_stores_the_row_under_the_canonical_handle() {
    let fx = mind_server::fixture("priority-canonical");
    with_pump_home(&fx);
    let (key, need) = operator_and_need(&fx);
    let answer = fx
        .call(
            &key,
            "pump_priority",
            json!({"action": "set", "need": &need[..8], "level": 4}),
        )
        .await;
    assert_eq!(
        answer,
        json!({"action": "set", "need": need, "level": 4}),
        "{answer}"
    );
    assert_eq!(stored(&fx), vec![(need, 4)]);
}

/// Setting again replaces the row; level 0 removes it; the need can be
/// prioritized again afterwards.
#[tokio::test]
async fn set_replaces_and_zero_level_clears_the_row() {
    let fx = mind_server::fixture("priority-zero-clears");
    with_pump_home(&fx);
    let (key, need) = operator_and_need(&fx);
    for (level, rows) in [
        (5, vec![(need.clone(), 5)]),
        (0, vec![]),
        (2, vec![(need.clone(), 2)]),
    ] {
        let answer = fx
            .call(
                &key,
                "pump_priority",
                json!({"action": "set", "need": &need, "level": level}),
            )
            .await;
        assert_eq!(
            answer,
            json!({"action": "set", "need": need, "level": level}),
            "{answer}"
        );
        assert_eq!(stored(&fx), rows, "level {level}");
    }
}

/// `clear` removes the row and answers the canonical need at level 0.
#[tokio::test]
async fn clear_removes_the_row_and_answers_the_canonical_need() {
    let fx = mind_server::fixture("priority-clear");
    with_pump_home(&fx);
    let (key, need) = operator_and_need(&fx);
    fx.call(
        &key,
        "pump_priority",
        json!({"action": "set", "need": &need, "level": 3}),
    )
    .await;
    let answer = fx
        .call(
            &key,
            "pump_priority",
            json!({"action": "clear", "need": &need[..8]}),
        )
        .await;
    assert_eq!(
        answer,
        json!({"action": "clear", "need": need, "level": 0}),
        "{answer}"
    );
    assert!(stored(&fx).is_empty(), "{:?}", stored(&fx));
}

/// The writes are live operator state: once the pump home's store exists at
/// the current schema — the running pump creates it at startup — they answer,
/// and repeat, while the pump holds the home's lock.
#[tokio::test]
async fn priority_writes_answer_while_the_pump_lock_is_held() {
    let fx = mind_server::fixture("priority-live-pump");
    with_pump_home(&fx);
    let (key, need) = operator_and_need(&fx);
    // The running pump owns startup: its first open creates pump.db at the
    // current schema. One admitted write plays that part here.
    fx.call(
        &key,
        "pump_priority",
        json!({"action": "set", "need": &need, "level": 1}),
    )
    .await;
    let _lock = hold_pump_lock(&fx);
    for (level, rows) in [(7, vec![(need.clone(), 7)]), (0, vec![])] {
        let answer = fx
            .call(
                &key,
                "pump_priority",
                json!({"action": "set", "need": &need, "level": level}),
            )
            .await;
        assert_eq!(
            answer,
            json!({"action": "set", "need": need, "level": level}),
            "{answer}"
        );
        assert_eq!(stored(&fx), rows, "level {level}");
    }
}

/// The writes stay operator-only: a key acting as a hand is refused and
/// stores nothing.
#[tokio::test]
async fn writes_need_an_operator_key() {
    let fx = mind_server::fixture("priority-operator-only");
    with_pump_home(&fx);
    let (_, need) = operator_and_need(&fx);
    let hand = fx.key_by(&fx.member, Some("hand"));
    for arguments in [
        json!({"action": "set", "need": &need, "level": 1}),
        json!({"action": "clear", "need": &need}),
    ] {
        let message = fx.refused(&hand, "pump_priority", arguments).await;
        assert!(!message.is_empty(), "{message}");
    }
    assert!(stored(&fx).is_empty(), "{:?}", stored(&fx));
}

/// The server's write audit log at the fixture's data dir: one JSON object
/// per line, one line per write tool call.
fn audit_lines(fx: &mind_server::Fx) -> Vec<Value> {
    fs::read_to_string(fx.data.join("audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Exactly one more audit line than `before`, for an authenticated
/// `pump_priority` write with `outcome`; `project` is `None` for a call
/// refused before its project argument resolved. Audit fields only: the
/// message text is never asserted.
fn assert_one_audit(fx: &mind_server::Fx, before: usize, outcome: &str, project: Option<&str>) {
    let lines = audit_lines(fx);
    assert_eq!(lines.len(), before + 1, "{lines:?}");
    let line = &lines[before];
    assert_eq!(line["tool"], "pump_priority", "{line}");
    assert_eq!(line["outcome"], outcome, "{line}");
    match project {
        Some(id) => assert_eq!(line["project_id"], json!(id), "{line}"),
        None => assert!(line["project_id"].is_null(), "{line}"),
    }
    assert!(line["key_id"].is_string(), "{line}");
    assert!(line["principal"].is_string(), "{line}");
    assert!(line["at"].is_string(), "{line}");
    assert!(
        line["summary"].as_str().is_some_and(|s| !s.is_empty()),
        "{line}"
    );
}

/// One raw `tools/call` POST that keeps the HTTP status, for refusals the
/// endpoint answers before any tool runs. The caller passes `project`.
async fn raw_call(
    fx: &mind_server::Fx,
    token: &str,
    name: &str,
    arguments: Value,
) -> (StatusCode, Value) {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments}
    });
    let response = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/mcp")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// A throwaway but well-formed ed25519 SPKI PEM, minted from `seed`.
fn spki_pem(seed: u8) -> String {
    use base64::Engine;
    const PREFIX: &[u8] = &[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = PREFIX.to_vec();
    der.extend_from_slice(
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .as_bytes(),
    );
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
        base64::engine::general_purpose::STANDARD.encode(der)
    )
}

/// The authorization matrix for the pump_priority writes, over the real
/// router, with one audit line per authenticated write call: the host's
/// own member's key acting as the operator role writes and is audited
/// exactly once as `ok`; a member bearer token is refused on the wire
/// (401, `unauthorized`) before any tool runs and writes no audit; a key
/// of a nonoperator role, a foreign member's key acting as operator, a key
/// with no grant on the project, and a granted key whose project is not
/// hosted are all refused as `isError` with one `refused` audit each —
/// asserted through wire flags, audit fields and database values, never
/// message text. After the operator role's kind is demoted with the
/// mailspace's own editor, the same previously minted operator key is
/// refused too: the rule is re-checked live per call, never from the
/// grant, and no refusal ever moves the stored row.
#[tokio::test]
async fn priority_writes_answer_only_the_live_operator_and_are_audited_once() {
    let fx = mind_server::fixture("priority-authorization-matrix");
    with_pump_home(&fx);
    let (operator, need) = operator_and_need(&fx);
    let hand = fx.key_by(&fx.member, Some("hand"));
    let bob_token = fx
        .state
        .identities
        .register(
            &fx.project,
            "bob",
            &spki_pem(9),
            "0b0b0b0b-1111-4222-8333-444444444444",
        )
        .unwrap()
        .token;
    let bobs = fx.key_by("bob", Some("operator"));
    let ungranted = fx
        .state
        .server_keys
        .mint(
            &format!("{}.ungranted@{}", fx.member, fx.project),
            false,
            Vec::new(),
            "test",
            &fx.member,
        )
        .unwrap()
        .key;
    let elsewhere = fx
        .state
        .projects
        .create_project_with_creator(&spki_pem(7), "alice")
        .unwrap()
        .project_id;
    fx.state
        .identities
        .register(
            &elsewhere,
            "alice",
            &spki_pem(7),
            "0a0a0a0a-1111-4222-8333-444444444444",
        )
        .ok();
    let alice = fx
        .state
        .server_keys
        .mint(
            &format!("alice.operator@{elsewhere}"),
            false,
            vec![GrantRecord {
                project_id: elsewhere.clone(),
                member: "alice".into(),
                identity: Some("operator".into()),
                level: GrantLevel::Operator,
            }],
            "test",
            "alice",
        )
        .unwrap()
        .key;

    // The one admitted principal: answered, stored, audited exactly once.
    let set = fx
        .call(
            &operator,
            "pump_priority",
            json!({"action": "set", "need": &need, "level": 3}),
        )
        .await;
    assert_eq!(
        set,
        json!({"action": "set", "need": need, "level": 3}),
        "{set}"
    );
    assert_eq!(stored(&fx), vec![(need.clone(), 3)]);
    let lines = audit_lines(&fx);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["summary"], "ok", "{lines:?}");
    assert_one_audit(&fx, 0, "ok", Some(&fx.project));

    // A member bearer token is refused on the wire before any tool runs:
    // the pre-authentication behavior, with no audit line at all.
    let (status, reply) = raw_call(
        &fx,
        &bob_token,
        "pump_priority",
        json!({"project": fx.project, "action": "set", "need": &need, "level": 4}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{reply}");
    assert_eq!(reply["error"]["code"], "unauthorized", "{reply}");
    assert_eq!(
        audit_lines(&fx).len(),
        1,
        "a pre-authentication refusal writes no audit: {:?}",
        audit_lines(&fx)
    );

    // Every other refused principal: one `refused` audit line each, with
    // the project id attached only where the grant resolved.
    let refusals = [
        (
            &hand,
            json!({"action": "set", "need": &need, "level": 1}),
            Some(fx.project.clone()),
        ),
        (
            &bobs,
            json!({"action": "clear", "need": &need}),
            Some(fx.project.clone()),
        ),
        (
            &ungranted,
            json!({"action": "set", "need": &need, "level": 1}),
            None,
        ),
        (
            &alice,
            json!({"project": elsewhere, "action": "set", "need": &need, "level": 1}),
            Some(elsewhere.clone()),
        ),
    ];
    for (token, arguments, project) in &refusals {
        let before = audit_lines(&fx).len();
        fx.refused(token, "pump_priority", arguments.clone()).await;
        assert_one_audit(&fx, before, "refused", project.as_deref());
    }
    assert_eq!(
        stored(&fx),
        vec![(need.clone(), 3)],
        "no refusal touched the row"
    );

    // Demote the operator role's kind: the previously minted operator key
    // is refused on the next call — the rule is read live per call.
    mind_server::open_root(&fx.root)
        .set_role(
            "operator",
            RoleUpdate {
                kind: Some(Some("hand".into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    let before = audit_lines(&fx).len();
    fx.refused(
        &operator,
        "pump_priority",
        json!({"action": "set", "need": &need, "level": 5}),
    )
    .await;
    assert_one_audit(&fx, before, "refused", Some(&fx.project));
    assert_eq!(stored(&fx), vec![(need, 3)], "{:?}", stored(&fx));
}

/// Per-key write exhaustion: with the write window at two, two priority
/// writes are admitted and audited `ok`, and the same key's later writes —
/// `set` and `clear` alike — are refused with one `refused` audit each and
/// leave the stored row unchanged, while a second operator key's window is
/// still its own.
#[tokio::test]
async fn priority_writes_exhaust_the_per_key_window_without_touching_the_row() {
    let fx = mind_server::fixture_with("priority-write-window", |state| {
        state.limiter = RateLimiter::new(Some(2));
    });
    with_pump_home(&fx);
    let (key, need) = operator_and_need(&fx);
    for level in [2, 3] {
        let answer = fx
            .call(
                &key,
                "pump_priority",
                json!({"action": "set", "need": &need, "level": level}),
            )
            .await;
        assert_eq!(
            answer,
            json!({"action": "set", "need": need, "level": level}),
            "{answer}"
        );
    }
    assert_eq!(stored(&fx), vec![(need.clone(), 3)]);
    assert_eq!(audit_lines(&fx).len(), 2, "{:?}", audit_lines(&fx));
    assert_one_audit(&fx, 1, "ok", Some(&fx.project));

    // The window is spent: refused on the wire, audited once, no row move.
    let before = audit_lines(&fx).len();
    fx.refused(
        &key,
        "pump_priority",
        json!({"action": "set", "need": &need, "level": 9}),
    )
    .await;
    assert_one_audit(&fx, before, "refused", Some(&fx.project));
    assert_eq!(
        stored(&fx),
        vec![(need.clone(), 3)],
        "the refused set must not touch the row"
    );
    let before = audit_lines(&fx).len();
    fx.refused(
        &key,
        "pump_priority",
        json!({"action": "clear", "need": &need}),
    )
    .await;
    assert_one_audit(&fx, before, "refused", Some(&fx.project));
    assert_eq!(
        stored(&fx),
        vec![(need.clone(), 3)],
        "the refused clear must not touch the row"
    );

    // The window is per key: a second operator key writes right away.
    let second = fx.key_by(&fx.member, Some("operator"));
    let answer = fx
        .call(
            &second,
            "pump_priority",
            json!({"action": "clear", "need": &need}),
        )
        .await;
    assert_eq!(
        answer,
        json!({"action": "clear", "need": need, "level": 0}),
        "{answer}"
    );
    assert_one_audit(&fx, audit_lines(&fx).len() - 1, "ok", Some(&fx.project));
    assert!(stored(&fx).is_empty(), "{:?}", stored(&fx));
}

/// Invalid levels and wrong-kind anchors are refused as writes with one
/// `refused` audit line each and leave the stored row alone; a set through
/// an unambiguous prefix answers and stores the canonical need key only.
#[tokio::test]
async fn invalid_levels_and_wrong_kinds_are_audited_and_store_nothing() {
    let fx = mind_server::fixture("priority-invalid-levels-audit");
    with_pump_home(&fx);
    let (key, need) = operator_and_need(&fx);
    let task = fx.send("mind", "task", "hand", "a unit", "+++\n+++\n\nunit body");
    let mail = fx.send("mind", "mail", "hand", "a letter", "hello");
    fx.call(
        &key,
        "pump_priority",
        json!({"action": "set", "need": &need, "level": 2}),
    )
    .await;
    assert_one_audit(&fx, 0, "ok", Some(&fx.project));
    let bad = [
        json!({"action": "set", "need": &need, "level": "2"}),
        json!({"action": "set", "need": &need, "level": -1}),
        json!({"action": "set", "need": &need, "level": 1.5}),
        json!({"action": "set", "need": &task, "level": 1}),
        json!({"action": "set", "need": &mail, "level": 1}),
        json!({"action": "set", "need": "no-such-handle", "level": 1}),
        json!({"action": "clear", "need": &task}),
    ];
    for arguments in &bad {
        let before = audit_lines(&fx).len();
        fx.refused(&key, "pump_priority", arguments.clone()).await;
        assert_one_audit(&fx, before, "refused", Some(&fx.project));
    }
    assert_eq!(
        stored(&fx),
        vec![(need.clone(), 2)],
        "no refusal touched the row"
    );
    let answer = fx
        .call(
            &key,
            "pump_priority",
            json!({"action": "set", "need": &need[..8], "level": 5}),
        )
        .await;
    assert_eq!(
        answer,
        json!({"action": "set", "need": need, "level": 5}),
        "{answer}"
    );
    let lines = audit_lines(&fx);
    assert_eq!(lines.len(), 2 + bad.len(), "{lines:?}");
    assert_eq!(lines.last().unwrap()["outcome"], "ok", "{lines:?}");
    assert_eq!(
        stored(&fx),
        vec![(need, 5)],
        "only the canonical need key is stored"
    );
}
