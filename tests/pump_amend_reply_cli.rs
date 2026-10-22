//! A real mail reply must move board `last_event` and start a plan revision.

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
fn a_real_reply_moves_board_last_event_and_starts_a_revision() {
    let project = temp_mailspace();
    let handle = send_need(&project);
    let (ctx, mut state) = planned_ctx(&project, &handle);

    detect(&ctx, &mut state);
    assert!(
        !state.needs[&handle].amendment_pending,
        "the first observation is a baseline, not a revision"
    );
    assert!(
        state.needs[&handle]
            .thread_sha256
            .as_deref()
            .is_some_and(|hash| hash.starts_with("v4:")),
        "baseline hash missing: {:?}",
        state.needs[&handle].thread_sha256
    );
    let before = board_need(&project, &handle);
    let before_event = before["last_event"].clone();

    assert_success(&vivi([
        "mail",
        "reply",
        &handle,
        "--from",
        "operator",
        "--body",
        "please add the second half",
        "--project",
        root(&project),
    ]));

    let after = board_need(&project, &handle);
    assert_eq!(after["last_event"]["command"], "mail reply");
    assert_eq!(after["last_event"]["event_type"], "replied");
    assert_ne!(
        after["last_event"]["occurred_at"],
        Value::Null,
        "reply last_event: {after}"
    );
    if before_event.is_object() {
        assert_ne!(
            after["last_event"]["occurred_at"], before_event["occurred_at"],
            "board last_event stayed {before_event}"
        );
    }

    detect(&ctx, &mut state);
    assert!(
        state.needs[&handle].amendment_pending,
        "revision not started; hash={:?} seen={:?} show={} board={} log={}",
        state.needs[&handle].thread_sha256,
        state.needs[&handle].thread_seen_at,
        stdout(&vivi([
            "need",
            "show",
            &handle,
            "--json",
            "--project",
            root(&project)
        ])),
        stdout(&vivi(["board", "--json", "--project", root(&project)])),
        fs::read_to_string(project.path().join(".vivi/pump/log.txt")).unwrap_or_default()
    );
    assert!(state.board.revision_planning.contains_key(&handle));
    let records = vivi([
        "pump",
        "list",
        "--need",
        &handle,
        "--json",
        "--project",
        root(&project),
    ]);
    assert_success(&records);
    let records: Value = serde_json::from_str(&stdout(&records)).unwrap();
    assert_eq!(
        records.as_array().map(Vec::len),
        Some(1),
        "the amendment is a dispatch with a pump record: {records}"
    );
    assert_eq!(records[0]["status"], "open");
}

#[test]
fn a_real_reply_reopens_a_closed_need_and_starts_a_revision() {
    let project = temp_mailspace();
    let handle = send_need(&project);
    let (ctx, mut state) = planned_ctx(&project, &handle);
    detect(&ctx, &mut state);

    assert_success(&vivi([
        "need",
        "done",
        &handle,
        "--for",
        "mind",
        "--project",
        root(&project),
    ]));
    state.closed_needs.push(handle.clone());

    assert_success(&vivi([
        "mail",
        "reply",
        &handle,
        "--from",
        "operator",
        "--body",
        "reopen this: add the missing half",
        "--project",
        root(&project),
    ]));

    detect(&ctx, &mut state);
    assert!(
        !state.closed_needs.contains(&handle),
        "closed_needs: {:?}",
        state.closed_needs
    );
    assert!(state.needs[&handle].amendment_pending);
    assert!(state.board.revision_planning.contains_key(&handle));
    let listed = vivi([
        "need",
        "list",
        "--for",
        "mind",
        "--status",
        "open",
        "--json",
        "--project",
        root(&project),
    ]);
    assert_success(&listed);
    let items: Value = serde_json::from_str(&stdout(&listed)).unwrap();
    assert!(
        items
            .as_array()
            .into_iter()
            .flatten()
            .any(|item| item["handle"] == handle),
        "reopened need missing from open list: {}",
        stdout(&listed)
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

fn board_need(project: &tempfile::TempDir, handle: &str) -> Value {
    let out = vivi(["board", "--json", "--project", root(project)]);
    assert_success(&out);
    let board: Value = serde_json::from_str(&stdout(&out)).unwrap();
    board["identities"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|identity| identity["needs"].as_array().into_iter().flatten())
        .find(|need| need["handle"] == handle)
        .cloned()
        .unwrap_or_else(|| panic!("need {handle} missing from board {}", stdout(&out)))
}

fn temp_mailspace() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    assert_success(&vivi(["mailspace", "init", "--project", root(&project)]));
    for (role, kind) in [("operator", "operator"), ("mind", "mind")] {
        let _ = vivi([
            "role",
            "add",
            role,
            "--kind",
            kind,
            "--project",
            root(&project),
        ]);
    }
    project
}

fn send_need(project: &tempfile::TempDir) -> String {
    let out = vivi([
        "need",
        "send",
        "--from",
        "operator",
        "--to",
        "mind",
        "--subject",
        "amendment trigger",
        "--body",
        "done_when: the first half exists.",
        "--project",
        root(project),
    ]);
    assert_success(&out);
    stdout(&out)
        .lines()
        .find_map(|line| line.strip_prefix("created mind ").map(str::to_owned))
        .unwrap_or_else(|| panic!("no handle in {}", stdout(&out)))
}

fn root(project: &tempfile::TempDir) -> &str {
    project.path().to_str().unwrap()
}

fn vivi<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
