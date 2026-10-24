//! Tests for [`crate::pump::board`]: the board pass pulls when due and pushes
//! what is pending, driven through [`FakeHost`] replies.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::pump::board::*;
use crate::pump::config::{BoardConfig, PumpConfig};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::State;

/// A context with an operator sender; three planners, 1800 s plan timeout.
fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    setup_with(BoardConfig {
        from: vec![String::from("operator@bd371d96.local")],
        ..BoardConfig::default()
    })
}

/// A context with `board` swapped into an otherwise default config.
fn setup_with(board: BoardConfig) -> (TempDir, Ctx, Arc<FakeHost>) {
    let cfg = PumpConfig {
        board,
        ..PumpConfig::default()
    };
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg);
    (dir, ctx, host)
}

/// One JSON row for a `need list` reply.
fn row(handle: &str, date: &str, subject: &str, marks: &[&str]) -> String {
    let marks = marks
        .iter()
        .map(|mark| format!("\"{mark}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{{\"handle\": \"{handle}\", \"date\": \"{date}\", \"subject\": \"{subject}\", \"marks\": [{marks}]}}"
    )
}

/// A JSON array reply wrapping `rows`.
fn list(rows: &[String]) -> String {
    format!("[{}]", rows.join(", "))
}

#[test]
fn parse_rows_skips_leading_text_and_reads_the_keys() {
    let out = format!(
        "note: 2 rows below\n{}\n",
        list(&[
            row("n1", "2026-10-14T01:00", "First", &["pump"]),
            String::from("{}")
        ])
    );
    let rows = parse_rows(&out).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].handle, "n1");
    assert_eq!(rows[0].date, "2026-10-14T01:00");
    assert_eq!(rows[0].subject, "First");
    assert_eq!(rows[0].marks, vec!["pump"]);
    assert_eq!(rows[1].handle, "");
    assert_eq!(rows[1].date, "");
    assert_eq!(rows[1].subject, "");
    assert!(rows[1].marks.is_empty());
}

#[test]
fn parse_rows_returns_none_for_garbage() {
    assert_eq!(parse_rows("no brackets here"), None);
    assert_eq!(parse_rows(""), None);
    assert_eq!(parse_rows("lead [ oops"), None);
    assert_eq!(parse_rows("lead {\"not\": \"an array\"}"), None);
}

#[test]
fn board_skips_a_pass_inside_the_every_window_and_pulls_otherwise() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.board.at = ctx.now();
    board(&ctx, &mut st);
    assert!(host.commands().is_empty());
    host.reply("sync pull", 0, "pulled 3 messages");
    host.advance(301);
    board(&ctx, &mut st);
    assert_eq!(st.board.at, ctx.now());
    assert!(
        host.logs()
            .contains(&"sync pull rc=0 pulled 3 messages".to_string())
    );
    assert_eq!(host.commands().len(), 1, "{:?}", host.commands());
    assert!(st.jobs.is_empty(), "the board pass plans nothing");
    assert!(!host.spawned().iter().any(|cmd| cmd.contains("sync push")));
}

#[test]
fn board_without_pull_runs_no_vivi_command_and_still_stamps() {
    let (_dir, ctx, host) = setup_with(BoardConfig {
        pull: false,
        ..BoardConfig::default()
    });
    let mut st = State::default();
    host.advance(301);
    board(&ctx, &mut st);
    assert!(host.commands().is_empty(), "{:?}", host.commands());
    assert_eq!(st.board.at, ctx.now());
}

#[test]
fn board_pushes_once_when_a_push_is_pending() {
    let (_dir, ctx, host) = setup();
    host.reply("sync pull", 0, "pulled 0 messages");
    let mut st = State::default();
    st.board.sync_pending = true;
    host.advance(301);
    board(&ctx, &mut st);
    assert!(!st.board.sync_pending);
    let pushes = host
        .spawned()
        .iter()
        .filter(|cmd| cmd.contains("sync push"))
        .count();
    assert_eq!(pushes, 1, "{:?}", host.spawned());
}

#[test]
fn a_slow_board_pass_waits_the_interval_from_completion() {
    let (_dir, ctx, host) = setup_with(BoardConfig {
        from: vec![String::from("operator@bd371d96.local")],
        every: 60,
        ..BoardConfig::default()
    });
    host.advance_on_run(30);
    host.reply("sync pull", 0, "pulled 0 messages");
    let mut st = State::default();

    board(&ctx, &mut st);

    let command_count = host.commands().len();
    assert!(ctx.now().saturating_sub(st.board.at) < ctx.cfg.board.every);
    board(&ctx, &mut st);
    assert_eq!(host.commands().len(), command_count);
}

#[test]
fn board_raises_sync_letter_quarantined_once_per_content_id() {
    let (dir, ctx, host) = setup();
    let project = dir.path().join("project");
    let first = "ab".repeat(32);
    let second = "cd".repeat(32);
    let dropped = "ee".repeat(32);
    let decoy = "ff".repeat(32);
    hold_receipt(
        &project,
        &first,
        r#"{"message_id":"m-1","from":"a@b","kind":"task","sequence":3,"reason":"unknown front key flavor","at":"2026-10-24T00:00:00Z"}"#,
    );
    let root = project.join(".vivi").join("quarantine");
    fs::write(root.join(format!("{first}.eml")), b"raw bytes").unwrap();
    fs::create_dir_all(root.join("dropped")).unwrap();
    fs::write(
        root.join("dropped").join(format!("{dropped}.json")),
        r#"{"reason":"already dropped"}"#,
    )
    .unwrap();
    host.reply(
        "sync pull",
        1,
        &format!("1 quarantined: {decoy} (from the child)"),
    );
    let mut st = State::default();

    board(&ctx, &mut st);

    let text = exceptions_text(&ctx);
    let first_lines = alarms_for(&text, &first);
    assert_eq!(first_lines.len(), 1, "{text}");
    assert!(
        first_lines[0].contains("unknown front key flavor"),
        "{text}"
    );
    assert_eq!(exception_logs(&host, &first), 1, "{:?}", host.logs());
    assert!(
        !text.contains(&decoy),
        "the child's text is not the alarm: {text}"
    );
    assert!(!text.contains(&dropped), "dropped/ is not scanned: {text}");
    assert_eq!(
        alarms_for(&text, SYNC_LETTER_QUARANTINED).len(),
        1,
        "{text}"
    );

    host.advance(301);
    board(&ctx, &mut st);
    assert_eq!(alarms_for(&exceptions_text(&ctx), &first).len(), 1);
    assert_eq!(exception_logs(&host, &first), 1, "{:?}", host.logs());

    hold_receipt(&project, &second, "{not json");
    host.advance(301);
    board(&ctx, &mut st);

    let text = exceptions_text(&ctx);
    assert_eq!(alarms_for(&text, &first).len(), 1, "{text}");
    let second_lines = alarms_for(&text, &second);
    assert_eq!(second_lines.len(), 1, "{text}");
    assert!(
        !second_lines[0].contains("quarantined:"),
        "a corrupt receipt still alarms, without a reason: {text}"
    );
    assert_eq!(exception_logs(&host, &second), 1, "{:?}", host.logs());
}

#[test]
fn board_does_not_scan_quarantine_when_the_pull_succeeds() {
    let (dir, ctx, host) = setup();
    let id = "ab".repeat(32);
    hold_receipt(
        &dir.path().join("project"),
        &id,
        r#"{"reason":"unknown front key flavor"}"#,
    );
    host.reply("sync pull", 0, "pulled 0 messages");
    let mut st = State::default();

    board(&ctx, &mut st);

    let text = exceptions_text(&ctx);
    assert!(
        !text.contains(SYNC_LETTER_QUARANTINED),
        "a zero exit does not scan: {text}"
    );
    assert!(
        !host
            .logs()
            .iter()
            .any(|line| line.contains(SYNC_LETTER_QUARANTINED)),
        "{:?}",
        host.logs()
    );
}

/// One `<project>/.vivi/quarantine/<id>.json` receipt.
fn hold_receipt(project: &Path, id: &str, body: &str) {
    let root = project.join(".vivi").join("quarantine");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join(format!("{id}.json")), body).unwrap();
}

/// The pump's `exceptions.txt`; empty when nothing was raised.
fn exceptions_text(ctx: &Ctx) -> String {
    fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap_or_default()
}

/// Lines that name both the quarantine alarm and `id`.
fn alarms_for<'a>(text: &'a str, id: &str) -> Vec<&'a str> {
    text.lines()
        .filter(|line| line.contains(SYNC_LETTER_QUARANTINED) && line.contains(id))
        .collect()
}

/// Log lines `EXCEPTION sync-letter-quarantined <id>`.
fn exception_logs(host: &FakeHost, id: &str) -> usize {
    let line = format!("EXCEPTION {SYNC_LETTER_QUARANTINED} {id}");
    host.logs().iter().filter(|logged| *logged == &line).count()
}

#[test]
fn a_board_stamp_in_the_future_skips_the_pass_and_does_not_underflow() {
    let (_dir, ctx, host) = setup_with(BoardConfig {
        every: 60,
        ..BoardConfig::default()
    });
    let mut st = State::default();
    let ahead = ctx.now() + 5_000;
    st.board.at = ahead;
    board(&ctx, &mut st);
    assert_eq!(st.board.at, ahead);
    assert!(host.commands().is_empty());
}
