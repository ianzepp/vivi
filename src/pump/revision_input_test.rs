//! Tests for [`crate::pump::revision_input`]: the thread gives way to the
//! budget, the prior plan does not.

use serde_json::{Value, json};
use tempfile::tempdir;

use super::*;
use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::state::NeedRecord;

fn state_with_unit(brief: &str) -> State {
    let mut st = State::default();
    st.needs.insert(
        "n1".into(),
        NeedRecord {
            units: vec!["n1-1".into()],
            revision: 1,
            ..NeedRecord::default()
        },
    );
    st.units.insert(
        "n1-1".into(),
        Unit {
            need: "n1".into(),
            status: Status::Pending,
            brief: brief.into(),
            ..Unit::default()
        },
    );
    st
}

fn message(id: &str, handle: &str, body: &str) -> Value {
    json!({"handle": handle, "message_id": id, "from": "operator@x", "body": body, "date": ""})
}

fn thread_of(messages: Vec<Value>) -> Thread {
    Thread::Messages(messages)
}

#[test]
fn a_tight_budget_drops_the_oldest_replies_before_the_need_or_anything_new() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let st = state_with_unit("brief");
    let mut messages = vec![message("m0", "n1", "the need itself")];
    for n in 1..=30 {
        messages.push(message(
            &format!("m{n}"),
            &format!("r{n}"),
            &"old ".repeat(800),
        ));
    }
    messages.push(message("m31", "r31", "the newest change"));
    let seen: BTreeSet<String> = (0..=30).map(|n| format!("m{n}")).collect();
    let thread = thread_of(messages);
    let inputs = Inputs {
        need: "n1",
        revision: 2,
        thread: &thread,
        seen: Some(&seen),
    };

    let out = build(&ctx, &st, &inputs, Some(20_000)).unwrap();

    assert!(out.to_string().len() <= 20_000, "{}", out.to_string().len());
    let bodies: Vec<&str> = out["thread"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies.first(), Some(&"the need itself"));
    assert_eq!(bodies.last(), Some(&"the newest change"));
    assert!(out["thread_elided"]["messages_left_out"].as_u64().unwrap() > 0);
    // Newest old replies survive before the oldest ones.
    let handles: Vec<&str> = out["thread"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["handle"].as_str().unwrap())
        .collect();
    assert!(handles.contains(&"r30"));
    assert!(!handles.contains(&"r1"));
    assert_eq!(out["units"][0]["brief"], "brief");
}

#[test]
fn an_over_long_new_message_is_cut_and_says_so() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let st = state_with_unit("brief");
    let thread = thread_of(vec![
        message("m0", "n1", "the need itself"),
        message("m1", "r1", &"long ".repeat(20_000)),
    ]);
    let seen: BTreeSet<String> = ["m0".to_string()].into();
    let inputs = Inputs {
        need: "n1",
        revision: 2,
        thread: &thread,
        seen: Some(&seen),
    };

    let out = build(&ctx, &st, &inputs, Some(12_000)).unwrap();

    assert!(out.to_string().len() <= 12_000);
    let cut = out["thread"][1]["body"].as_str().unwrap();
    assert!(cut.contains("[body cut: kept"), "{cut}");
    assert_eq!(out["thread_elided"]["bodies_cut"], 1);
    // The full copy keeps everything.
    let full = build(&ctx, &st, &inputs, None).unwrap();
    assert_eq!(full["thread"][1]["body"].as_str().unwrap().len(), 100_000);
}

#[test]
fn closed_units_give_up_their_briefs_before_a_plan_is_refused() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let mut st = state_with_unit("live brief");
    st.units.insert(
        "n1-2".into(),
        Unit {
            need: "n1".into(),
            status: Status::Withdrawn,
            brief: "w".repeat(30_000),
            ..Unit::default()
        },
    );
    st.needs.get_mut("n1").unwrap().units.push("n1-2".into());
    let thread = thread_of(vec![message("m0", "n1", "the need itself")]);
    let inputs = Inputs {
        need: "n1",
        revision: 2,
        thread: &thread,
        seen: None,
    };

    let out = build(&ctx, &st, &inputs, Some(20_000)).unwrap();

    assert_eq!(out["units"][0]["brief"], "live brief");
    assert_eq!(out["units"][1]["brief_elided"], true);
    assert!(out["units"][1]["brief"].as_str().unwrap().len() < 200);
}
