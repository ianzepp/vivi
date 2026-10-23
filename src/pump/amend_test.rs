//! Tests for [`crate::pump::amend`]: detecting a thread change and asking
//! for a revision, and doing nothing when the thread is unchanged.

use std::fs;
use std::sync::Arc;

use tempfile::tempdir;

use crate::pump::amend::request;
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
fn an_oversized_thread_is_cut_but_the_prior_plan_is_kept_whole() {
    let (_dir, ctx, host, mut st) = setup();
    let thread = format!(
        "## thread\n{}",
        "operator said \"more\" here\n".repeat(10_000)
    );
    host.reply("need show n1", 0, &thread);
    let brief = format!("{}END-OF-BRIEF", "keep this brief whole. ".repeat(900));
    st.units.get_mut("n1-1").unwrap().brief = brief.clone();

    request(&ctx, &mut st, "n1", "thread grew");

    let body = &revision_job(&st).body;
    assert!(body.len() < 56 * 1024, "{} bytes", body.len());
    assert!(body.contains("REVISION-INPUT-BEGIN"));
    assert!(body.contains("REVISION-INPUT-END"));
    assert!(body.contains("Pool result contract"));
    assert!(body.contains("Reason for this revision: thread grew"));
    // The brief is the planner's to keep verbatim: never cut.
    let input = revision_input_json(body);
    assert_eq!(input["units"][0]["brief"], brief);
    // The thread gave way, and the input says so.
    assert!(
        input["thread"]
            .as_str()
            .unwrap()
            .contains("middle of the thread left out")
    );
    assert!(
        input["thread_elided"]["text_bytes_left_out"]
            .as_u64()
            .unwrap()
            > 0
    );
    // The full input stays on the pump host.
    let saved = fs::read_to_string(ctx.home.join("needs/n1.json")).unwrap();
    assert!(saved.len() > 250_000);
    assert!(st.board.revision_planning.contains_key("n1"));
}

#[test]
fn a_prior_plan_too_big_for_the_letter_is_refused_not_cut() {
    let (_dir, ctx, _host, mut st) = setup();
    st.units.get_mut("n1-1").unwrap().status = Status::Pending;
    st.units.get_mut("n1-1").unwrap().brief = "x".repeat(60_000);

    request(&ctx, &mut st, "n1", "thread grew");

    assert!(st.jobs.is_empty());
    assert!(!st.board.revision_planning.contains_key("n1"));
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exceptions.contains("revision-input n1"), "{exceptions}");
    assert!(exceptions.contains("prior plan of n1"), "{exceptions}");
    // The full input is still saved for the operator.
    assert!(ctx.home.join("needs/n1.json").exists());
}

/// The input JSON between the markers of a revision prompt.
fn revision_input_json(body: &str) -> serde_json::Value {
    let begin = body.find("REVISION-INPUT-BEGIN\n").unwrap() + "REVISION-INPUT-BEGIN\n".len();
    let end = body.find("\nREVISION-INPUT-END").unwrap();
    serde_json::from_str(&body[begin..end]).unwrap()
}

fn unit_with(need: &str, status: Status, brief: &str) -> Unit {
    Unit {
        need: need.into(),
        status,
        revision: 1,
        brief: brief.into(),
        write_scope: vec![String::from("src/")],
        ..Unit::default()
    }
}

#[test]
fn a_revision_input_labels_every_unit_and_keeps_every_brief_whole() {
    let (_dir, ctx, host, mut st) = setup();
    let long = format!("{}TAIL", "w".repeat(5_000));
    for (uid, status) in [
        ("n1-2", Status::Withdrawn),
        ("n1-3", Status::Pending),
        ("n1-4", Status::Inflight),
        ("n1-5", Status::Replaced),
    ] {
        st.units.insert(uid.into(), unit_with("n1", status, &long));
        st.needs.get_mut("n1").unwrap().units.push(uid.into());
    }
    // A unit a past revision withdrew is no longer in the need record, but
    // the planner must still see that it is closed.
    st.units
        .insert("n1-6".into(), unit_with("n1", Status::Withdrawn, "dropped"));
    st.units.insert(
        "other-1".into(),
        unit_with("other", Status::Pending, "elsewhere"),
    );
    host.reply("need show n1", 0, "## thread\n");

    request(&ctx, &mut st, "n1", "addendum");

    let input = revision_input_json(&revision_job(&st).body);
    let units = input["units"].as_array().unwrap();
    let label = |id: &str| {
        let unit = units.iter().find(|u| u["id"] == id).unwrap();
        (
            unit["state"].as_str().unwrap(),
            unit["amendable"].as_bool().unwrap(),
        )
    };
    assert_eq!(label("n1-1"), ("merged", false));
    assert_eq!(label("n1-2"), ("withdrawn", false));
    assert_eq!(label("n1-3"), ("pending", true));
    assert_eq!(label("n1-4"), ("in-flight", false));
    assert_eq!(label("n1-5"), ("replaced", false));
    assert_eq!(label("n1-6"), ("withdrawn", false));
    assert!(units.iter().all(|u| u["id"] != "other-1"));
    for uid in ["n1-2", "n1-3", "n1-4", "n1-5"] {
        let unit = units.iter().find(|u| u["id"] == uid).unwrap();
        assert_eq!(unit["brief"], long, "{uid}");
    }
    let prompt = &revision_job(&st).body;
    assert!(prompt.contains("never change a withdrawn or replaced unit"));
    assert!(prompt.contains("amend only units whose state is `pending`"));
}

const PLANNED_THREAD: &str = r#"[
    {"handle":"n1","message_id":"m1","from":"operator@12345678.local","subject":"Do the thing","body":"original need","date":"2026-10-17T10:00:00+00:00"},
    {"handle":"p1","message_id":"m2","from":"pump@12345678.local","body":"pump: planned. verdict: units; revision 1; 1 unit(s).","date":"2026-10-17T10:05:00+00:00"},
    {"handle":"e1","message_id":"m3","from":"pump@12345678.local","body":"pump: attempt status event","event":"marked","date":"2026-10-17T10:06:00+00:00"},
    {"handle":"r1","message_id":"m4","from":"operator@12345678.local","body":"add the second half","date":"2026-10-17T11:00:00+00:00"}
]"#;

/// [`PLANNED_THREAD`] with the pump's own messages sent by the configured
/// pump identity.
fn planned_thread(ctx: &Ctx) -> String {
    PLANNED_THREAD.replace(
        "pump@12345678.local",
        &format!("{}@12345678.local", ctx.cfg.reply_from),
    )
}

#[test]
fn the_thread_marks_what_changed_since_the_last_plan_reply() {
    let (_dir, ctx, host, mut st) = setup();
    script_thread_json(&host, &planned_thread(&ctx));

    request(&ctx, &mut st, "n1", "addendum");

    let input = revision_input_json(&revision_job(&st).body);
    let messages = input["thread"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert_eq!(messages[0]["kind"], "need");
    assert_eq!(messages[0]["since_last_plan"], false);
    assert_eq!(messages[1]["body"], "add the second half");
    assert_eq!(messages[1]["since_last_plan"], true);
    // The lifecycle event and the pump's own reply are named, not silently gone.
    assert_eq!(
        input["thread_elided"]["lifecycle_event_letters_left_out"],
        1
    );
    assert_eq!(input["thread_elided"]["pump_replies_left_out"], 1);
    assert!(
        input["thread_note"]
            .as_str()
            .unwrap()
            .contains("after the pump's last planned/revised reply")
    );
}

#[test]
fn the_next_revision_marks_messages_the_last_planner_was_not_shown() {
    let (_dir, ctx, host, mut st) = setup();
    script_thread_json(&host, &planned_thread(&ctx));
    request(&ctx, &mut st, "n1", "first ask");
    // The planner of r2 saw m1..m4; the plan landed, then a reply arrived.
    st.needs.get_mut("n1").unwrap().revision = 2;
    st.needs.get_mut("n1").unwrap().amendment_pending = false;
    st.board.revision_planning.clear();
    let later = planned_thread(&ctx).replace(
        "\n]",
        ",\n    {\"handle\":\"r2\",\"message_id\":\"m5\",\"from\":\"operator@12345678.local\",\"body\":\"one more thing\",\"date\":\"2026-10-17T12:00:00+00:00\"}\n]",
    );
    script_thread_json(&host, &later);
    host.set_now(5_000);

    request(&ctx, &mut st, "n1", "second ask");

    let job = st
        .jobs
        .values()
        .filter(|job| job.kind == "planner:revision")
        .max_by_key(|job| job.queued_at)
        .unwrap();
    let input = revision_input_json(&job.body);
    let flags: Vec<(String, bool)> = input["thread"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["body"].as_str().unwrap().to_owned(),
                m["since_last_plan"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        flags,
        [
            ("original need".to_owned(), false),
            ("add the second half".to_owned(), false),
            ("one more thing".to_owned(), true),
        ]
    );
    assert!(input["thread_note"].as_str().unwrap().contains("not shown"));
}

fn script_thread_json(host: &FakeHost, thread: &str) {
    host.reply("need show n1", 0, "## thread\n");
    host.reply("need show n1 --json", 0, thread);
}

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

/// What a pool planner returns for a revision: the plan between the markers,
/// the way a real worker's final message carries it.
fn pool_receipt(plan: &serde_json::Value, fenced: bool) -> String {
    let json = serde_json::to_string_pretty(plan).unwrap();
    let body = if fenced {
        format!("```json\n{json}\n```")
    } else {
        json
    };
    format!(
        "agent_rc 0 agent_s 163\nhand's final message:\nverified against the checkout\n\
         PUMP-PLAN-BEGIN\n{body}\nPUMP-PLAN-END\n"
    )
}

/// The pool result of a revision planner that does not repeat the revision
/// number: hub planners returned `{need, verdict, reason, withdraw, units}`
/// with no `revision` field, and the answer was consumed as stale and lost
/// without a trace while the need waited on a revision that had landed.
#[test]
fn a_pool_planner_answer_with_no_revision_field_lands_as_the_requested_revision() {
    for fenced in [false, true] {
        let (_dir, ctx, host, mut st) = setup();
        request(&ctx, &mut st, "n1", "gate fix needs another unit");
        let plan = serde_json::json!({
            "need": "n1",
            "verdict": "units",
            "reason": "one more unit",
            "withdraw": [],
            "units": [{
                "id": "n1-2",
                "brief": "do the other half",
                "write_scope": ["src/"],
                "status": "pending"
            }],
        });
        finish_revision(&mut st, &pool_receipt(&plan, fenced));

        planner_jobs::apply_completions(&ctx, &mut st);
        crate::pump::plan::ingest(&ctx, &mut st);

        assert_eq!(st.needs["n1"].revision, 2, "fenced={fenced}");
        assert!(!st.needs["n1"].amendment_pending, "fenced={fenced}");
        assert!(st.units.contains_key("n1-2"), "fenced={fenced}");
        assert!(
            host.logs().iter().any(|l| l.contains("revise n1 r2")),
            "fenced={fenced}"
        );
    }
}
