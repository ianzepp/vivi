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
            name: "pump-planner".into(),
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

fn source_hash(ctx: &Ctx, body: &str) -> String {
    let messages = [serde_json::json!({
        "message_id": "m1",
        "from": "operator@12345678.local",
        "body": body,
    })];
    super::amend::source_thread_hash(ctx, "n1", &messages).unwrap()
}

fn legacy_hash(ctx: &Ctx, messages: &[serde_json::Value]) -> String {
    let mut hasher = Sha256::new();
    for message in messages {
        let from = message["from"].as_str().unwrap_or("");
        let sender = if ctx.cfg.reply_from.contains('@') {
            from
        } else {
            from.split('@').next().unwrap_or(from)
        };
        if sender.eq_ignore_ascii_case(&ctx.cfg.reply_from) {
            continue;
        }
        hasher.update(message["message_id"].as_str().unwrap_or("").as_bytes());
        hasher.update(b"\n");
        hasher.update(message["body"].as_str().unwrap_or("").as_bytes());
        hasher.update(b"\n");
    }
    format!("v2:{}", hex::encode(hasher.finalize()))
}

#[test]
fn a_changed_thread_starts_a_revision() {
    let (_dir, ctx, host, mut st) = setup();
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(source_hash(&ctx, "original need"));
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
fn a_reply_starts_a_revision_even_when_board_last_event_does_not_move() {
    let (_dir, ctx, host, mut st) = setup();
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(source_hash(&ctx, "original need"));
    st.needs.get_mut("n1").unwrap().thread_seen_at = Some(1_000);
    host.reply(
        "board --json",
        0,
        r#"{"identities":[{"needs":[{"handle":"n1","last_event":{"occurred_at":"1970-01-01T00:16:40+00:00"}}]}]}"#,
    );
    host.reply("need show n1", 0, "## thread\n");
    host.reply(
        "need show n1 --json",
        0,
        r#"[
            {"handle":"n1","message_id":"m1","from":"operator@12345678.local","body":"original need","date":"1970-01-01T00:16:40+00:00"},
            {"handle":"r1","message_id":"m2","from":"operator@12345678.local","body":"add the second half","date":"2026-10-17T13:00:00+00:00"}
        ]"#,
    );
    detect(&ctx, &mut st);
    assert!(st.needs["n1"].amendment_pending);
    assert!(st.board.revision_planning.contains_key("n1"));
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
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(source_hash(&ctx, "same body"));
    script_thread(&host, "same body");
    detect(&ctx, &mut st);
    assert!(!st.needs["n1"].amendment_pending);
    assert!(!st.board.revision_planning.contains_key("n1"));
}

#[test]
fn an_unchanged_thread_keeps_the_need_closed() {
    let (_dir, ctx, host, mut st) = setup();
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(source_hash(&ctx, "same body"));
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
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(source_hash(&ctx, "original need"));
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
        let prior = [serde_json::json!({
            "message_id": "m1",
            "from": "operator@12345678.local",
            "body": "original need",
        })];
        st.needs.get_mut("n1").unwrap().thread_sha256 = Some(legacy_hash(&ctx, &prior));
        script_thread(&host, "original need");
        detect(&ctx, &mut st);
        let hash = st.needs["n1"].thread_sha256.clone();
        st.needs.get_mut("n1").unwrap().thread_seen_at = Some(1_000);
        host.reply(
            "need show n1 --json",
            0,
            r#"[
                {"message_id":"m1","from":"operator@12345678.local","body":"original need"},
                {"message_id":"m2","from":"mind@12345678.local","body":"pump: attempt record\noutcome: delivered"}
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
            {"message_id":"m2","from":"mind@12345678.local","body":"pump: attempt record\noutcome: delivered"},
            {"message_id":"m3","from":"operator@12345678.local","body":"also handle the edge case"}
        ]"#,
    );
    detect(&ctx, &mut st);
    assert!(st.needs["n1"].amendment_pending);
    assert!(st.board.revision_planning.contains_key("n1"));
    assert_eq!(st.jobs.len(), 1);
}

#[test]
fn an_operator_reply_quoting_the_pump_marker_still_amends() {
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
            {"message_id":"m2","from":"operator@12345678.local","body":"pump: this is an operator quote"}
        ]"#,
    );

    detect(&ctx, &mut st);

    assert!(st.needs["n1"].amendment_pending);
    assert!(st.board.revision_planning.contains_key("n1"));
}

#[test]
fn a_changed_thread_is_hashed_even_when_thread_seen_at_is_already_ahead() {
    let (_dir, ctx, host, mut st) = setup();
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(source_hash(&ctx, "original need"));
    st.needs.get_mut("n1").unwrap().thread_seen_at = Some(u64::MAX);
    script_thread(&host, "add the second half");
    detect(&ctx, &mut st);
    assert!(st.needs["n1"].amendment_pending);
    assert!(
        host.commands()
            .iter()
            .any(|cmd| cmd.contains("need show n1 --json"))
    );
}

#[test]
fn an_amendment_is_not_detected_twice() {
    let (_dir, ctx, host, mut st) = setup();
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(source_hash(&ctx, "original need"));
    script_thread(&host, "add the second half");
    detect(&ctx, &mut st);
    assert!(st.needs["n1"].amendment_pending);
    let queued = st.jobs.len();
    detect(&ctx, &mut st);
    assert_eq!(st.jobs.len(), queued);
}

#[test]
fn an_unknown_hash_is_used_as_a_new_baseline_not_read_as_an_amendment() {
    let (_dir, ctx, host, mut st) = setup();
    // This value is neither valid v2 nor v3 and cannot prove a prior snapshot.
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(String::from("0123abcd"));
    script_thread(&host, "a body the old hash never saw");
    detect(&ctx, &mut st);
    assert!(!st.needs["n1"].amendment_pending);
    assert!(!st.board.revision_planning.contains_key("n1"));
    let adopted = st.needs["n1"].thread_sha256.clone().unwrap();
    assert!(adopted.starts_with("v4:"), "{adopted}");
}

#[test]
fn a_changed_operator_thread_with_a_valid_v2_hash_starts_a_revision() {
    let (_dir, ctx, host, mut st) = setup();
    let prior = [serde_json::json!({
        "message_id": "m1",
        "from": "operator@12345678.local",
        "body": "original need",
    })];
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(legacy_hash(&ctx, &prior));
    script_thread(&host, "add the second half");

    detect(&ctx, &mut st);

    assert!(st.needs["n1"].amendment_pending);
    assert!(st.board.revision_planning.contains_key("n1"));
    assert!(
        st.needs["n1"]
            .thread_sha256
            .as_deref()
            .unwrap()
            .starts_with("v4:")
    );
}

#[test]
fn a_matching_v2_thread_hash_upgrades_to_v3_without_replanning() {
    let (_dir, ctx, host, mut st) = setup();
    let prior = [serde_json::json!({
        "message_id": "m1",
        "from": "operator@12345678.local",
        "body": "same body",
    })];
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(legacy_hash(&ctx, &prior));
    script_thread(&host, "same body");

    detect(&ctx, &mut st);

    assert!(!st.needs["n1"].amendment_pending);
    assert!(st.board.revision_planning.is_empty());
    assert!(
        st.needs["n1"]
            .thread_sha256
            .as_deref()
            .unwrap()
            .starts_with("v4:")
    );
}

#[test]
fn a_mind_authored_reply_missing_from_v2_still_starts_a_revision() {
    let (_dir, ctx, host, mut st) = setup();
    let prior = [serde_json::json!({
        "handle": "n1",
        "message_id": "root",
        "from": "operator@12345678.local",
        "body": "original need",
    })];
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(legacy_hash(&ctx, &prior));
    host.reply(
        "board --json",
        0,
        r#"{"identities":[{"needs":[{"handle":"n1","last_event":{"occurred_at":"2026-10-17T13:00:00+00:00"}}]}]}"#,
    );
    host.reply("need show n1", 0, "## thread\n");
    host.reply(
        "need show n1 --json",
        0,
        r#"[
            {"handle":"n1","message_id":"root","from":"operator@12345678.local","body":"original need"},
            {"handle":"n1-reply","message_id":"mind-reply","from":"mind@12345678.local","body":"done_when: add the missing behavior"}
        ]"#,
    );

    detect(&ctx, &mut st);

    assert!(st.needs["n1"].amendment_pending);
    assert!(st.board.revision_planning.contains_key("n1"));
}

#[test]
fn mind_authored_need_root_is_hashed_and_a_mind_amendment_reopens_the_plan() {
    let (_dir, ctx, host, mut st) = setup();
    host.reply(
        "board --json",
        0,
        r#"{"identities":[{"needs":[{"handle":"n1","last_event":{"occurred_at":"2026-10-17T13:00:00+00:00"}}]}]}"#,
    );
    host.reply("need show n1", 0, "## thread\n");
    host.reply(
        "need show n1 --json",
        0,
        r#"[{"handle":"n1","message_id":"root","from":"mind@12345678.local","body":"Need\ndone_when: original requirement"}]"#,
    );
    detect(&ctx, &mut st);
    assert!(
        st.needs["n1"]
            .thread_sha256
            .as_deref()
            .unwrap()
            .starts_with("v4:")
    );
    assert!(!st.needs["n1"].amendment_pending);

    host.reply(
        "board --json",
        0,
        r#"{"identities":[{"needs":[{"handle":"n1","last_event":{"occurred_at":"2026-10-17T13:01:00+00:00"}}]}]}"#,
    );
    host.reply(
        "need show n1 --json",
        0,
        r#"[
            {"handle":"n1","message_id":"root","from":"mind@12345678.local","body":"Need\ndone_when: original requirement"},
            {"handle":"n1-r","message_id":"pump-reply","from":"mind@12345678.local","body":"pump: planned. verdict: units; revision 1; 1 unit(s)."},
            {"handle":"n1-a","message_id":"mind-amendment","from":"mind@12345678.local","body":"done_when: added requirement"}
        ]"#,
    );
    detect(&ctx, &mut st);

    assert!(st.needs["n1"].amendment_pending);
    assert!(st.board.revision_planning.contains_key("n1"));
}

#[test]
fn an_oversized_thread_and_plan_keep_the_revision_prompt_under_the_letter_cap() {
    let (_dir, ctx, host, mut st) = setup();
    let thread = format!(
        "## thread\n{}",
        "operator said \"more\" here\n".repeat(10_000)
    );
    host.reply("need show n1", 0, &thread);
    st.units.get_mut("n1-1").unwrap().brief = "x".repeat(60_000);

    request(&ctx, &mut st, "n1", "thread grew");

    let body = &revision_job(&st).body;
    assert!(body.len() < 56 * 1024, "{} bytes", body.len());
    assert!(body.contains("REVISION-INPUT-BEGIN"));
    assert!(body.contains("REVISION-INPUT-END"));
    assert!(body.contains("Pool result contract"));
    assert!(body.contains("[truncated; full thread via `vivi need show n1`"));
    assert!(body.contains("Reason for this revision: thread grew"));
    // The full input stays on the pump host for the marker to point at.
    let saved = fs::read_to_string(ctx.home.join("needs/n1.json")).unwrap();
    assert!(saved.len() > 250_000);
    assert!(st.board.revision_planning.contains_key("n1"));
}

fn script_thread_json(host: &FakeHost, thread: &str) {
    host.reply("need show n1", 0, "## thread\n");
    host.reply("need show n1 --json", 0, thread);
}

const EVENT_THREAD: &str = r#"[
    {"message_id":"m1","from":"operator@12345678.local","body":"original need","event":null},
    {"handle":"e1","message_id":"m2","from":"mind@12345678.local","body":"unmarked","event":"unmarked"},
    {"handle":"e2","message_id":"m3","from":"operator@12345678.local","body":"Superseded: done","event":"done"}
]"#;

#[test]
fn event_letters_never_enter_the_source_hash() {
    let (_dir, ctx, _host, _st) = setup();
    let plain = [serde_json::json!({"handle":"n1","message_id":"m1","from":"o","body":"x"})];
    let with_event = [
        plain[0].clone(),
        serde_json::json!({"handle":"e","message_id":"m2","from":"o","body":"y","event":"done"}),
    ];
    assert_eq!(
        super::amend::source_thread_hash(&ctx, "n1", &plain),
        super::amend::source_thread_hash(&ctx, "n1", &with_event)
    );
}

#[test]
fn event_letters_do_not_start_a_revision() {
    let (_dir, ctx, host, mut st) = setup();
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(source_hash(&ctx, "original need"));
    script_thread_json(&host, EVENT_THREAD);
    detect(&ctx, &mut st);
    assert!(!st.needs["n1"].amendment_pending);
    assert!(st.board.revision_planning.is_empty());
}

#[test]
fn a_stored_v3_hash_is_re_baselined_without_a_revision() {
    let (_dir, ctx, host, mut st) = setup();
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(format!("v3:{}", "b".repeat(64)));
    script_thread_json(&host, EVENT_THREAD);
    detect(&ctx, &mut st);
    assert!(!st.needs["n1"].amendment_pending);
    assert!(st.jobs.is_empty());
    assert_eq!(
        st.needs["n1"].thread_sha256,
        Some(source_hash(&ctx, "original need"))
    );
}

#[test]
fn a_withdrawn_need_is_neither_reopened_nor_revised() {
    let (_dir, ctx, host, mut st) = setup();
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(source_hash(&ctx, "original need"));
    st.needs.get_mut("n1").unwrap().withdrawn = true;
    st.closed_needs.push(String::from("n1"));
    script_thread(&host, "add the second half");
    detect(&ctx, &mut st);
    assert!(st.closed_needs.contains(&String::from("n1")));
    assert!(!st.needs["n1"].amendment_pending);
    assert!(st.jobs.is_empty());
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("thread changed while withdrawn; not reopened"))
    );
    assert!(
        !host
            .commands()
            .iter()
            .any(|cmd| cmd.contains("need reopen"))
    );
}

#[test]
fn a_need_closed_in_the_mailspace_is_re_baselined_not_revised() {
    let (_dir, ctx, host, mut st) = setup();
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(source_hash(&ctx, "original need"));
    host.reply("need list --status done", 0, r#"[{"handle":"n1"}]"#);
    script_thread(&host, "add the second half");
    detect(&ctx, &mut st);
    assert!(!st.needs["n1"].amendment_pending);
    assert!(st.jobs.is_empty());
    assert_eq!(
        st.needs["n1"].thread_sha256,
        Some(source_hash(&ctx, "add the second half"))
    );
}

#[test]
fn a_pump_closed_need_done_in_the_mailspace_still_reopens_for_an_operator_reply() {
    let (_dir, ctx, host, mut st) = setup();
    st.needs.get_mut("n1").unwrap().thread_sha256 = Some(source_hash(&ctx, "original need"));
    st.closed_needs.push(String::from("n1"));
    host.reply("need list --status done", 0, r#"[{"handle":"n1"}]"#);
    script_thread(&host, "add the second half");
    detect(&ctx, &mut st);
    assert!(st.needs["n1"].amendment_pending);
    assert!(!st.closed_needs.contains(&String::from("n1")));
}

/// Mark the revision job of `n1` finished with the given receipt.
fn finish_revision(st: &mut State, receipt: &str) {
    let queued_at = st.board.revision_planning["n1"];
    let id = planner_jobs::job_id("revision", "n1.revision", queued_at);
    let job = st.jobs.get_mut(&id).expect("revision job");
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: "agent_rc 0".into(),
        receipt: receipt.into(),
        at: queued_at + 1,
    });
}

#[test]
fn a_requested_revision_nobody_is_answering_is_asked_again() {
    let (_dir, ctx, host, mut st) = setup();
    st.needs.get_mut("n1").unwrap().amendment_pending = true;
    script_thread(&host, "same body");

    detect(&ctx, &mut st);

    assert!(st.board.revision_planning.contains_key("n1"));
    assert!(
        revision_job(&st)
            .body
            .contains("previous revision planner gave no usable plan")
    );
    assert_eq!(st.board.planner_fails["n1.revision"], 1);
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("n1 has a requested revision and no planner"))
    );
}

#[test]
fn a_failed_revision_is_asked_again_after_a_backoff_and_then_given_up() {
    let (_dir, ctx, host, mut st) = setup();
    request(&ctx, &mut st, "n1", "unit n1-1 failed its proof");
    finish_revision(
        &mut st,
        "agent_rc 127 agent_s 0\nhand's final message:\nConnection error.\n",
    );
    planner_jobs::apply_completions(&ctx, &mut st);
    script_thread(&host, "same body");

    // The pass that sees the failure raises it and waits out the backoff.
    detect(&ctx, &mut st);
    assert!(!st.board.revision_planning.contains_key("n1"));
    assert!(st.needs["n1"].amendment_pending);
    assert_eq!(st.board.planner_fails["n1.revision"], 1);
    let retry_at = st.board.planner_retry_at["n1.revision"];
    detect(&ctx, &mut st);
    assert!(!st.board.revision_planning.contains_key("n1"));

    // Once the backoff is over the planner is asked again.
    host.set_now(retry_at);
    detect(&ctx, &mut st);
    assert!(st.board.revision_planning.contains_key("n1"));
    assert_eq!(st.board.planner_fails["n1.revision"], 2);

    // After the last try the need waits for an operator, with one exception.
    st.board.revision_planning.clear();
    st.board
        .planner_fails
        .insert(String::from("n1.revision"), 3);
    st.board.planner_retry_at.clear();
    detect(&ctx, &mut st);
    detect(&ctx, &mut st);
    assert!(st.board.revision_planning.is_empty());
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert_eq!(
        exceptions.matches("revision-failed n1").count(),
        1,
        "{exceptions}"
    );
    assert!(exceptions.contains("vivi pump replan n1"), "{exceptions}");
}

#[test]
fn an_answer_waiting_to_be_ingested_is_not_asked_for_again() {
    let (_dir, ctx, host, mut st) = setup();
    st.needs.get_mut("n1").unwrap().amendment_pending = true;
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(ctx.home.join("plans/n1.r2.json"), "{}").unwrap();
    script_thread(&host, "same body");

    detect(&ctx, &mut st);

    assert!(st.board.revision_planning.is_empty());
    assert!(st.jobs.is_empty());
}

#[test]
fn an_answer_consumed_without_effect_is_replaced_by_the_next_ask() {
    let (_dir, ctx, host, mut st) = setup();
    st.needs.get_mut("n1").unwrap().amendment_pending = true;
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(ctx.home.join("plans/n1.r2.json"), "{}").unwrap();
    st.plans_seen.push(String::from("n1.r2.json"));
    script_thread(&host, "same body");

    detect(&ctx, &mut st);

    assert!(st.board.revision_planning.contains_key("n1"));
    assert!(!ctx.home.join("plans/n1.r2.json").exists());
    assert!(!st.plans_seen.iter().any(|seen| seen == "n1.r2.json"));
}
