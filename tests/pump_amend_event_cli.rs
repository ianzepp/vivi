//! Lifecycle event letters (mark, unmark, done, reopen) are state, not thread
//! content: they never start an amendment. A withdrawn need and a need closed
//! in the mailspace are never reopened or revised by the amendment scan.

use std::fs;
use std::process::{Command, Output};
use std::sync::Arc;

use serde_json::Value;
use vivi::pump::amend::detect;
use vivi::pump::config::PumpConfig;
use vivi::pump::host::{Ctx, RealHost};
use vivi::pump::state::{NeedRecord, State, Status, Unit};
use vivi::routing::{RoleRouting, RoutedPool, RoutedRole};

#[test]
fn need_show_json_names_event_letters_and_leaves_replies_null() {
    let project = temp_mailspace();
    let handle = send_need(&project);
    run(
        &project,
        &["need", "mark", &handle, "pump", "--for", "mind"],
    );
    run(
        &project,
        &["need", "unmark", &handle, "pump", "--for", "mind"],
    );
    run(
        &project,
        &[
            "need",
            "done",
            &handle,
            "--for",
            "mind",
            "--note",
            "superseded",
        ],
    );
    run(&project, &["need", "reopen", &handle, "--for", "mind"]);
    run(
        &project,
        &[
            "mail", "reply", &handle, "--from", "operator", "--body", "ordinary",
        ],
    );

    let shown = run(&project, &["need", "show", &handle, "--json"]);
    let thread: Vec<Value> = serde_json::from_str(&shown).unwrap();
    let event_of = |message: &Value| {
        assert!(message.get("event").is_some(), "no event key: {message}");
        message["event"].as_str().map(str::to_owned)
    };
    let root = thread.iter().find(|m| m["handle"] == handle).unwrap();
    assert_eq!(event_of(root), None);
    let reply = thread
        .iter()
        .find(|m| m["body"].as_str().is_some_and(|b| b.contains("ordinary")))
        .unwrap();
    assert_eq!(event_of(reply), None);
    let mut events: Vec<String> = thread.iter().filter_map(event_of).collect();
    events.sort();
    assert_eq!(events, ["done", "marked", "reopened", "unmarked"]);
}

#[test]
fn the_hub_event_sequence_never_amends_and_a_real_reply_still_does() {
    let project = temp_mailspace();
    let handle = send_need(&project);
    let (ctx, mut state) = planned_ctx(&project, &handle);
    detect(&ctx, &mut state);
    let baseline = state.needs[&handle].thread_sha256.clone();
    assert!(baseline.as_deref().is_some_and(|h| h.starts_with("v4:")));

    run(
        &project,
        &["need", "mark", &handle, "pump", "--for", "mind"],
    );
    run(
        &project,
        &["need", "unmark", &handle, "pump", "--for", "mind"],
    );
    detect(&ctx, &mut state);
    assert_quiet(&state, &handle, &baseline, "unmark");

    run(
        &project,
        &[
            "need",
            "done",
            &handle,
            "--for",
            "mind",
            "--note",
            "Superseded at the hub switch-over",
        ],
    );
    state.closed_needs.push(handle.clone());
    detect(&ctx, &mut state);
    assert_quiet(&state, &handle, &baseline, "done with note");
    assert!(state.closed_needs.contains(&handle), "reopened on an event");

    run(&project, &["need", "reopen", &handle, "--for", "mind"]);
    detect(&ctx, &mut state);
    assert_quiet(&state, &handle, &baseline, "reopen");

    run(&project, &["need", "done", &handle, "--for", "mind"]);
    run(
        &project,
        &[
            "mail",
            "reply",
            &handle,
            "--from",
            "operator",
            "--body",
            "add the missing half",
        ],
    );
    detect(&ctx, &mut state);
    assert!(state.needs[&handle].amendment_pending);
    assert!(state.board.revision_planning.contains_key(&handle));
    assert!(!state.closed_needs.contains(&handle), "not reopened");
}

#[test]
fn the_reopen_letter_the_pump_writes_does_not_feed_the_next_revision() {
    let project = temp_mailspace();
    let handle = send_need(&project);
    let (ctx, mut state) = planned_ctx(&project, &handle);
    detect(&ctx, &mut state);
    run(&project, &["need", "done", &handle, "--for", "mind"]);
    state.closed_needs.push(handle.clone());
    run(
        &project,
        &[
            "mail",
            "reply",
            &handle,
            "--from",
            "operator",
            "--body",
            "add the missing half",
        ],
    );
    detect(&ctx, &mut state);
    assert!(state.board.revision_planning.contains_key(&handle));
    let after_launch = state.needs[&handle].thread_sha256.clone();

    // The revision lands: the planner state clears and the scan runs again.
    state.board.revision_planning.remove(&handle);
    state.needs.get_mut(&handle).unwrap().amendment_pending = false;
    detect(&ctx, &mut state);
    assert_quiet(&state, &handle, &after_launch, "pump reopen letter");
}

#[test]
fn a_withdrawn_need_is_never_reopened_or_revised() {
    let project = temp_mailspace();
    let handle = send_need(&project);
    let (ctx, mut state) = planned_ctx(&project, &handle);
    detect(&ctx, &mut state);
    state.needs.get_mut(&handle).unwrap().withdrawn = true;
    state.closed_needs.push(handle.clone());
    run(
        &project,
        &[
            "mail",
            "reply",
            &handle,
            "--from",
            "operator",
            "--body",
            "one more thing",
        ],
    );
    let before = state.needs[&handle].thread_sha256.clone();
    detect(&ctx, &mut state);
    assert!(!state.needs[&handle].amendment_pending);
    assert!(state.board.revision_planning.is_empty());
    assert!(state.closed_needs.contains(&handle));
    assert_ne!(
        state.needs[&handle].thread_sha256, before,
        "not re-baselined"
    );
    let log = fs::read_to_string(project.path().join(".vivi/pump/log.txt")).unwrap();
    assert!(
        log.contains("thread changed while withdrawn; not reopened"),
        "{log}"
    );

    detect(&ctx, &mut state);
    assert!(state.board.revision_planning.is_empty());
}

#[test]
fn a_need_closed_in_the_mailspace_by_an_operator_is_not_revised() {
    let project = temp_mailspace();
    let handle = send_need(&project);
    let (ctx, mut state) = planned_ctx(&project, &handle);
    detect(&ctx, &mut state);
    run(&project, &["need", "done", &handle, "--for", "mind"]);
    run(
        &project,
        &[
            "mail",
            "reply",
            &handle,
            "--from",
            "operator",
            "--body",
            "late remark",
        ],
    );
    detect(&ctx, &mut state);
    assert!(!state.needs[&handle].amendment_pending);
    assert!(state.board.revision_planning.is_empty());
    let done = run(
        &project,
        &[
            "need", "list", "--for", "mind", "--status", "done", "--json",
        ],
    );
    assert!(done.contains(&handle), "need was reopened: {done}");
}

#[test]
fn a_stored_v3_hash_is_re_baselined_without_a_revision() {
    let project = temp_mailspace();
    let handle = send_need(&project);
    let (ctx, mut state) = planned_ctx(&project, &handle);
    run(
        &project,
        &["need", "mark", &handle, "pump", "--for", "mind"],
    );
    run(
        &project,
        &["need", "done", &handle, "--for", "mind", "--note", "note"],
    );
    state.needs.get_mut(&handle).unwrap().thread_sha256 = Some(format!("v3:{}", "a".repeat(64)));
    detect(&ctx, &mut state);
    assert!(!state.needs[&handle].amendment_pending);
    assert!(state.board.revision_planning.is_empty());
    let hash = state.needs[&handle].thread_sha256.clone().unwrap();
    assert!(hash.starts_with("v4:"), "{hash}");
}

fn assert_quiet(state: &State, handle: &str, baseline: &Option<String>, step: &str) {
    assert!(!state.needs[handle].amendment_pending, "{step} amended");
    assert!(state.board.revision_planning.is_empty(), "{step} revised");
    assert_eq!(
        &state.needs[handle].thread_sha256, baseline,
        "{step} moved the hash"
    );
}

fn planned_ctx(project: &tempfile::TempDir, handle: &str) -> (Ctx, State) {
    let home = project.path().join(".vivi/pump");
    fs::create_dir_all(home.join("plans")).unwrap();
    let cfg = PumpConfig {
        from: String::from("operator"),
        to: String::from("hand"),
        reply_from: String::from("mind"),
        ..PumpConfig::default()
    };
    let ctx = Ctx::new(
        cfg,
        Arc::new(RealHost::new(&home)),
        project.path().to_path_buf(),
        env!("CARGO_BIN_EXE_vivi").to_string(),
    )
    .with_routing(planner_routing());
    let mut state = State::default();
    state.needs.insert(
        handle.to_owned(),
        NeedRecord {
            verdict: String::from("units"),
            units: vec![format!("{handle}-1")],
            revision: 1,
            ..NeedRecord::default()
        },
    );
    state.units.insert(
        format!("{handle}-1"),
        Unit {
            need: handle.to_owned(),
            status: Status::Pending,
            revision: 1,
            ..Unit::default()
        },
    );
    (ctx, state)
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

fn temp_mailspace() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    run(&project, &["mailspace", "init"]);
    for (role, kind) in [("operator", "operator"), ("mind", "mind")] {
        let _ = vivi(&project, &["role", "add", role, "--kind", kind]);
    }
    project
}

fn send_need(project: &tempfile::TempDir) -> String {
    let out = run(
        project,
        &[
            "need",
            "send",
            "--from",
            "operator",
            "--to",
            "mind",
            "--subject",
            "event trigger",
            "--body",
            "done_when: the first half exists.",
        ],
    );
    out.lines()
        .find_map(|line| line.strip_prefix("created mind ").map(str::to_owned))
        .unwrap_or_else(|| panic!("no handle in {out}"))
}

fn vivi(project: &tempfile::TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .args(["--project", project.path().to_str().unwrap()])
        .output()
        .unwrap()
}

fn run(project: &tempfile::TempDir, args: &[&str]) -> String {
    let output = vivi(project, args);
    assert!(
        output.status.success(),
        "{args:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}
