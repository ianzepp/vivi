//! Tests for [`crate::pump::amend`]: detecting a thread change and asking
//! for a revision, and doing nothing when the thread is unchanged.

use std::fs;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tempfile::tempdir;

use crate::pump::amend::{detect, request};
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus};
use crate::pump::planner_jobs;
use crate::pump::state::{NeedRecord, State, Status, Unit};
use crate::routing::{RoleRouting, RoutedPool, RoutedRole};

fn setup() -> (tempfile::TempDir, Ctx, Arc<FakeHost>, State) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let ctx = ctx.with_routing(planner_routing());
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

fn planner_routing() -> RoleRouting {
    RoleRouting {
        roles: vec![RoutedRole {
            name: "planner".into(),
            kind: Some("planner".into()),
            status: "active".into(),
            pool: Some("here".into()),
            read_only: None,
            model: None,
            models: Vec::new(),
        }],
        pools: std::collections::BTreeMap::from([(
            "here".into(),
            RoutedPool {
                slots: 1,
                provider: "local".into(),
                fallback: None,
                caps: std::collections::BTreeMap::new(),
                models: Vec::new(),
                has_bands: false,
            },
        )]),
    }
}

fn revision_job(st: &State) -> &crate::pump::jobs::PoolJob {
    st.jobs
        .values()
        .find(|job| job.kind == "planner:revision")
        .expect("revision job")
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
    assert!(
        fs::read_to_string(ctx.home.join("needs/n1.json"))
            .unwrap()
            .contains("\"revision\": 2")
    );
    assert!(revision_job(&st).body.contains("REVISION-INPUT-BEGIN"));
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("amendment: n1 changed after r1"))
    );
}

#[test]
fn a_requested_revision_names_the_reason_in_the_prompt() {
    let (_dir, ctx, _host, mut st) = setup();
    request(&ctx, &mut st, "n1", "unit n1-1 failed its proof");
    assert!(st.needs["n1"].amendment_pending);
    assert!(ctx.home.join("needs/n1.json").exists());
    assert!(
        revision_job(&st)
            .body
            .contains("Reason for this revision: unit n1-1 failed its proof")
    );
}

#[test]
fn an_immediate_revision_exit_is_reported_without_waiting_for_plan_timeout() {
    let (_dir, ctx, host, mut st) = setup();
    request(&ctx, &mut st, "n1", "unit n1-1 failed its proof");
    let queued_at = st.board.revision_planning["n1"];
    let id = planner_jobs::job_id("revision", "n1.revision", queued_at);
    let job = st.jobs.get_mut(&id).expect("revision job");
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: "agent_rc 127".into(),
        receipt: "agent_rc 127 agent_s 0\nhand's final message:\nConnection error.\n".into(),
        at: queued_at + 1,
    });
    planner_jobs::apply_completions(&ctx, &mut st);
    script_thread(&host, "same body");
    detect(&ctx, &mut st);

    assert!(!st.board.revision_planning.contains_key("n1"));
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exceptions.contains("revision-no-answer n1"), "{exceptions}");
    assert!(exceptions.contains("rc=127"), "{exceptions}");
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
fn an_unchanged_thread_keeps_the_need_closed() {
    let (_dir, ctx, host, mut st) = setup();
    let mut hasher = Sha256::new();
    hasher.update(b"m1\n");
    hasher.update(b"same body\n");
    st.needs.get_mut("n1").unwrap().thread_sha256 =
        Some(format!("v2:{}", hex::encode(hasher.finalize())));
    st.closed_needs.push(String::from("n1"));
    script_thread(&host, "same body");
    detect(&ctx, &mut st);
    assert!(st.closed_needs.iter().any(|closed| closed == "n1"));
    assert!(!st.needs["n1"].amendment_pending);
    assert!(st.board.revision_planning.is_empty());
    assert!(st.jobs.is_empty());
    assert!(st.needs["n1"].thread_seen_at.unwrap() > 1_000);
}

#[test]
fn a_closed_need_with_a_changed_thread_reopens_and_plans_a_revision() {
    let (_dir, ctx, host, mut st) = setup();
    st.closed_needs.push(String::from("n1"));
    script_thread(&host, "add the second half");
    detect(&ctx, &mut st);
    assert!(!st.closed_needs.contains(&String::from("n1")));
    assert!(st.needs["n1"].amendment_pending);
    assert!(st.board.revision_planning.contains_key("n1"));
    assert!(
        fs::read_to_string(ctx.home.join("needs/n1.json"))
            .unwrap()
            .contains("\"revision\": 2")
    );
    assert!(revision_job(&st).body.contains("REVISION-INPUT-BEGIN"));
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("amendment: n1 reopened"))
    );
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
        assert!(st.jobs.is_empty());
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
    assert_eq!(st.jobs.len(), 1);
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
    let queued = st.jobs.len();
    detect(&ctx, &mut st);
    assert_eq!(st.jobs.len(), queued);
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
