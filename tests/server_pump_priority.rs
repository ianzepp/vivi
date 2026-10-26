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

#[path = "support/mind_server.rs"]
mod mind_server;

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use vivi::pump::control::project_home;
use vivi::pump::priority;

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
