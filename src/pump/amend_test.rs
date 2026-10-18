//! Tests for [`crate::pump::amend`]: detecting a thread change and asking
//! for a revision, and doing nothing when the thread is unchanged.

use std::fs;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tempfile::tempdir;

use crate::pump::amend::detect;
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{NeedRecord, State, Status, Unit};

fn setup() -> (tempfile::TempDir, Ctx, Arc<FakeHost>, State) {
    let dir = tempdir().unwrap();
    let mut cfg = PumpConfig::default();
    cfg.planner.run = String::from("planner-model");
    let (ctx, host) = test_ctx(dir.path(), cfg);
    let mut st = State::default();
    st.needs.insert(
        String::from("n1"),
        NeedRecord {
            verdict: String::from("units"),
            units: vec![String::from("n1-1")],
            revision: 1,
            thread_sha256: Some(String::from("v2:old")),
            thread_seen_at: Some(1_000),
            ..NeedRecord::default()
        },
    );
    st.units.insert(
        String::from("n1-1"),
        Unit {
            need: String::from("n1"),
            status: Status::Merged,
            revision: 1,
            ..Unit::default()
        },
    );
    (dir, ctx, host, st)
}

fn script_thread(host: &FakeHost, body: &str) {
    host.reply(
        "board --json",
        0,
        r#"{"identities":[{"needs":[{"handle":"n1","last_event":{"occurred_at":"2026-10-17T13:00:00+00:00"}}]}]}"#,
    );
    host.reply("need show n1", 0, "## thread\n");
    host.reply(
        "need show n1 --json",
        0,
        &format!(r#"[{{"message_id":"m1","from":"operator@12345678.local","body":"{body}"}}]"#),
    );
}

#[test]
fn a_changed_thread_starts_a_revision() {
    let (_dir, ctx, host, mut st) = setup();
    script_thread(&host, "add the second half");
    detect(&ctx, &mut st);
    assert!(st.needs["n1"].amendment_pending);
    assert!(st.board.revision_planning.contains_key("n1"));
    assert!(ctx.home.join("board/n1.revision.prompt").exists());
    assert!(
        fs::read_to_string(ctx.home.join("needs/n1.json"))
            .unwrap()
            .contains("\"revision\": 2")
    );
    assert!(
        host.spawned()
            .iter()
            .any(|cmd| cmd.contains("planner-model"))
    );
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("amendment: n1 changed after r1"))
    );
}

#[test]
fn an_unchanged_thread_does_not_amend() {
    let (_dir, ctx, host, mut st) = setup();
    let mut hasher = Sha256::new();
    hasher.update(b"m1\n");
    hasher.update(b"same body\n");
    st.needs.get_mut("n1").unwrap().thread_sha256 =
        Some(format!("v2:{}", hex::encode(hasher.finalize())));
    script_thread(&host, "same body");
    detect(&ctx, &mut st);
    assert!(!st.needs["n1"].amendment_pending);
    assert!(!st.board.revision_planning.contains_key("n1"));
}

#[test]
fn a_pump_attempt_record_reply_does_not_amend() {
    for reply_from in ["mind", "mind@12345678.local"] {
        let (_dir, mut ctx, host, mut st) = setup();
        Arc::make_mut(&mut ctx.cfg).reply_from = reply_from.to_owned();
        st.needs.get_mut("n1").unwrap().thread_sha256 = None;
        script_thread(&host, "original need");
        detect(&ctx, &mut st);
        let hash = st.needs["n1"].thread_sha256.clone();
        st.needs.get_mut("n1").unwrap().thread_seen_at = Some(1_000);
        host.reply(
            "need show n1 --json",
            0,
            r#"[
                {"message_id":"m1","from":"operator@12345678.local","body":"original need"},
                {"message_id":"m2","from":"mind@12345678.local","body":"Attempt record: delivered"}
            ]"#,
        );
        detect(&ctx, &mut st);
        assert_eq!(st.needs["n1"].thread_sha256, hash);
        assert!(!st.needs["n1"].amendment_pending);
        assert!(st.needs["n1"].thread_seen_at.unwrap() > 1_000);
        assert!(host.spawned().is_empty());
    }
}

#[test]
fn an_operator_reply_still_amends_after_a_pump_reply() {
    let (_dir, ctx, host, mut st) = setup();
    st.needs.get_mut("n1").unwrap().thread_sha256 = None;
    script_thread(&host, "original need");
    detect(&ctx, &mut st);
    st.needs.get_mut("n1").unwrap().thread_seen_at = Some(1_000);
    host.reply(
        "need show n1 --json",
        0,
        r#"[
            {"message_id":"m1","from":"operator@12345678.local","body":"original need"},
            {"message_id":"m2","from":"mind@12345678.local","body":"Attempt record: delivered"},
            {"message_id":"m3","from":"operator@12345678.local","body":"also handle the edge case"}
        ]"#,
    );
    detect(&ctx, &mut st);
    assert!(st.needs["n1"].amendment_pending);
    assert!(st.board.revision_planning.contains_key("n1"));
    assert_eq!(host.spawned().len(), 1);
}

#[test]
fn a_thread_seen_after_the_event_is_not_fetched() {
    let (_dir, ctx, host, mut st) = setup();
    st.needs.get_mut("n1").unwrap().thread_seen_at = Some(u64::MAX);
    script_thread(&host, "add the second half");
    detect(&ctx, &mut st);
    assert!(!st.needs["n1"].amendment_pending);
    assert!(
        !host
            .commands()
            .iter()
            .any(|cmd| cmd.contains("need show n1 --json"))
    );
}

#[test]
fn an_amendment_is_not_detected_twice() {
    let (_dir, ctx, host, mut st) = setup();
    script_thread(&host, "add the second half");
    detect(&ctx, &mut st);
    assert!(st.needs["n1"].amendment_pending);
    let spawned = host.spawned().len();
    detect(&ctx, &mut st);
    assert_eq!(host.spawned().len(), spawned);
}

#[test]
fn a_hash_from_the_older_scheme_is_adopted_not_read_as_an_amendment() {
    let (_dir, ctx, host, mut st) = setup();
    // Stored before the pump's own replies were left out of the hash.
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(String::from("0123abcd"));
    script_thread(&host, "a body the old hash never saw");
    detect(&ctx, &mut st);
    assert!(!st.needs["n1"].amendment_pending);
    assert!(!st.board.revision_planning.contains_key("n1"));
    let adopted = st.needs["n1"].thread_sha256.clone().unwrap();
    assert!(adopted.starts_with("v2:"), "{adopted}");
}
