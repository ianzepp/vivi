//! Tests for [`crate::pump::board`]: candidate and marked need rows, the
//! skip list, planner expiry and planner launch, driven through
//! [`FakeHost`] replies.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;
use tempfile::{TempDir, tempdir};

use crate::pump::board::*;
use crate::pump::config::{BoardConfig, PlannerConfig, PumpConfig};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{NeedRecord, State, Status, Unit};

/// A context with an operator sender and the `since` cutoff set; default
/// mark `pump`, three planners, 1800 s plan timeout.
fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    setup_with(BoardConfig {
        from: vec![String::from("operator@bd371d96.local")],
        since: String::from("2026-10-14T00:10"),
        ..BoardConfig::default()
    })
}

/// A context with `board` swapped into an otherwise default config.
fn setup_with(board: BoardConfig) -> (TempDir, Ctx, Arc<FakeHost>) {
    let cfg = PumpConfig {
        board,
        branch: String::from("next"),
        planner: PlannerConfig {
            run: String::from("planner-run {charter}"),
            ..PlannerConfig::default()
        },
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

/// A [`NeedRow`] to hand to [`launch_planners`] directly.
fn need_row(handle: &str, date: &str, subject: &str) -> NeedRow {
    NeedRow {
        handle: String::from(handle),
        date: String::from(date),
        subject: String::from(subject),
        marks: Vec::new(),
    }
}

/// The recorded exception lines (`<home>/exceptions.txt`).
fn exceptions(ctx: &Ctx) -> String {
    fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap_or_default()
}

/// A parked need record with a verdict, a reason and no units.
fn parked(verdict: &str, reason: &str) -> NeedRecord {
    NeedRecord {
        verdict: String::from(verdict),
        reason: String::from(reason),
        units: Vec::new(),
        ..NeedRecord::default()
    }
}

/// An operator thread header line and body, as `need show` prints them.
fn thread(stamp: &str, hex: &str, from: &str, body: &str) -> String {
    format!("## {stamp} - {hex} [open]\nFrom: {from}\n\n{body}\n")
}

/// A merged unit with a title and a commit.
fn merged_unit(title: &str, commit: &str) -> Unit {
    Unit {
        title: String::from(title),
        status: Status::Merged,
        commit: Some(String::from(commit)),
        ..Unit::default()
    }
}

/// Epoch seconds for an RFC 3339 stamp.
fn epoch(stamp: &str) -> u64 {
    u64::try_from(
        chrono::DateTime::parse_from_rfc3339(stamp)
            .unwrap()
            .timestamp(),
    )
    .unwrap()
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
fn marked_rows_keeps_only_rows_with_the_mark_and_deduplicates() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "--held",
        0,
        &list(&[
            row("n1", "2026-10-15", "One", &["pump"]),
            row("n2", "2026-10-15", "Two", &["other"]),
            row("n1", "2026-10-15", "One again", &["pump"]),
        ]),
    );
    host.reply(
        "--json --to mind",
        0,
        &list(&[row("n3", "2026-10-15", "Three", &["pump"])]),
    );
    let rows = marked_rows(&ctx);
    let handles: Vec<&str> = rows.iter().map(|row| row.handle.as_str()).collect();
    assert_eq!(handles, vec!["n1", "n3"]);
    assert_eq!(rows[0].subject, "One");
    assert_eq!(rows[1].subject, "Three");
}

#[test]
fn marked_rows_logs_an_unreadable_list_and_returns_what_it_could_read() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "--held",
        1,
        "error: the mailspace is locked\nsecond line of noise",
    );
    host.reply(
        "--json --to mind",
        0,
        &list(&[row("n3", "2026-10-15", "Three", &["pump"])]),
    );
    let rows = marked_rows(&ctx);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].handle, "n3");
    let logs = host.logs().join("\n");
    assert!(
        logs.contains(
            "marked list unreadable (need list --status open --json --held): \
             error: the mailspace is locked | second line of noise"
        ),
        "logs: {logs}"
    );
}

#[test]
fn candidate_rows_unions_senders_and_marked_rows_and_reports_the_approved_set() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "need list --from operator@bd371d96.local",
        0,
        &list(&[
            row("a", "2026-10-15", "A", &[]),
            row("b", "2026-10-15", "B", &["pump"]),
        ]),
    );
    host.reply(
        "--held",
        0,
        &list(&[
            row("c", "2026-10-15", "C", &["pump"]),
            row("b", "2026-10-15", "B", &["pump"]),
        ]),
    );
    host.reply(
        "--json --to mind",
        0,
        &list(&[row("d", "2026-10-15", "D", &["pump"])]),
    );
    let (rows, approved) = candidate_rows(&ctx);
    let handles: Vec<&str> = rows.iter().map(|row| row.handle.as_str()).collect();
    assert_eq!(handles, vec!["a", "b", "c", "d"]);
    let want: BTreeSet<String> = ["b", "c", "d"].iter().map(|h| (*h).to_owned()).collect();
    assert_eq!(approved, want);
    assert!(
        host.commands().iter().any(|cmd| cmd
            .contains("need list --from operator@bd371d96.local --to mind --status open --json")),
        "commands: {:?}",
        host.commands()
    );
}

#[test]
fn candidate_rows_plans_only_needs_addressed_to_the_reply_identity() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "need list --from operator@bd371d96.local",
        0,
        &list(&[row("other", "2026-10-15", "To hand", &[])]),
    );
    host.reply(
        "need list --from operator@bd371d96.local --to mind",
        0,
        &list(&[row("mine", "2026-10-15", "To mind", &[])]),
    );
    host.reply("--held", 0, "[]");
    host.reply("--json --to mind", 0, "[]");

    let (rows, _) = candidate_rows(&ctx);
    let handles: Vec<&str> = rows.iter().map(|row| row.handle.as_str()).collect();
    assert_eq!(handles, vec!["mine"]);
    assert!(
        host.commands()
            .iter()
            .filter(|cmd| cmd.contains("need list --from operator@bd371d96.local"))
            .all(|cmd| cmd.contains("--to mind")),
        "an unfiltered sender query is never issued: {:?}",
        host.commands()
    );
}

#[test]
fn read_skip_reads_whitespace_separated_handles() {
    let (_dir, ctx, _host) = setup();
    assert_eq!(read_skip(&ctx), BTreeSet::new());
    fs::write(ctx.home.join("skip.txt"), "n1 n2\n  n3\n").unwrap();
    let want: BTreeSet<String> = ["n1", "n2", "n3"].iter().map(|h| (*h).to_owned()).collect();
    assert_eq!(read_skip(&ctx), want);
}

#[test]
fn expire_planning_drops_entries_with_a_plan_and_raises_plan_no_answer_for_stale_ones() {
    let (_dir, ctx, _host) = setup();
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(ctx.home.join("plans/planned.json"), "{}").unwrap();
    let mut st = State::default();
    st.board
        .planning
        .insert(String::from("planned"), ctx.now() - 10);
    st.board
        .planning
        .insert(String::from("stale"), ctx.now() - 1801);
    st.board
        .planning
        .insert(String::from("fresh"), ctx.now() - 60);
    expire_planning(&ctx, &mut st);
    assert!(!st.board.planning.contains_key("planned"));
    assert!(!st.board.planning.contains_key("stale"));
    assert_eq!(st.board.planning.get("fresh"), Some(&(ctx.now() - 60)));
    let exc = exceptions(&ctx);
    assert!(
        exc.contains("plan-no-answer stale the planner wrote no plan in 30 minutes"),
        "exceptions: {exc}"
    );
    assert!(!exc.contains("planned "));
}

#[test]
fn a_need_dated_before_since_is_not_planned_unless_marked_or_replanned() {
    let (_dir, ctx, _host) = setup();
    let mut st = State::default();
    let mut replan: BTreeSet<String> = ["old-queued"].iter().map(|h| (*h).to_owned()).collect();
    let approved = BTreeSet::new();
    let rows = vec![
        need_row("old", "2026-10-13T00:00", "Old"),
        need_row("old-queued", "2026-10-13T00:00", "Old queued"),
        need_row("newer", "2026-10-15T00:00", "Newer"),
    ];
    launch_planners(&ctx, &mut st, &rows, &approved, &mut replan);
    assert!(!st.board.planning.contains_key("old"));
    assert!(st.board.planning.contains_key("old-queued"));
    assert!(st.board.planning.contains_key("newer"));
    assert!(!replan.contains("old-queued"));
    assert!(st.board.replan.is_empty());
}

#[test]
fn a_marked_need_is_planned_whatever_its_sender_or_date() {
    let (_dir, ctx, _host) = setup();
    let mut st = State::default();
    let mut replan = BTreeSet::new();
    let mut approved = BTreeSet::new();
    approved.insert(String::from("m1"));
    let rows = vec![need_row("m1", "2026-10-01T00:00", "Ancient but approved")];
    launch_planners(&ctx, &mut st, &rows, &approved, &mut replan);
    assert!(st.board.planning.contains_key("m1"));
}

#[test]
fn a_known_or_skipped_or_already_planning_need_is_not_planned() {
    let (_dir, ctx, _host) = setup();
    fs::write(ctx.home.join("skip.txt"), "skipped").unwrap();
    let mut st = State::default();
    st.needs
        .insert(String::from("known"), NeedRecord::default());
    st.board
        .planning
        .insert(String::from("inflight"), ctx.now());
    let rows = vec![
        need_row("known", "2026-10-15", "Known"),
        need_row("skipped", "2026-10-15", "Skipped"),
        need_row("inflight", "2026-10-15", "Inflight"),
        need_row("fresh", "2026-10-15", "Fresh"),
    ];
    let approved = BTreeSet::new();
    let mut replan = BTreeSet::new();
    launch_planners(&ctx, &mut st, &rows, &approved, &mut replan);
    assert_eq!(st.board.planning.len(), 2);
    assert!(st.board.planning.contains_key("inflight"));
    assert!(st.board.planning.contains_key("fresh"));
}

#[test]
fn planners_max_caps_concurrent_planners() {
    let (_dir, ctx, _host) = setup_with(BoardConfig {
        planners_max: 2,
        ..BoardConfig::default()
    });
    let mut st = State::default();
    st.board.planning.insert(String::from("p1"), ctx.now());
    st.board.planning.insert(String::from("p2"), ctx.now());
    let rows = vec![
        need_row("p3", "2026-10-15", "Third"),
        need_row("p4", "2026-10-15", "Fourth"),
    ];
    let approved = BTreeSet::new();
    let mut replan = BTreeSet::new();
    launch_planners(&ctx, &mut st, &rows, &approved, &mut replan);
    assert_eq!(st.board.planning.len(), 2);
    assert!(!st.board.planning.contains_key("p3"));
    assert!(!st.board.planning.contains_key("p4"));
}

#[test]
fn a_launched_planner_gets_the_need_file_and_the_prompt_and_is_recorded() {
    let (_dir, ctx, host) = setup();
    host.reply("need show n1", 0, &"x".repeat(40_010));
    let mut st = State::default();
    let subject = "S".repeat(80);
    let rows = vec![need_row("n1", "2026-10-15", &subject)];
    let approved = BTreeSet::new();
    let mut replan = BTreeSet::new();
    launch_planners(&ctx, &mut st, &rows, &approved, &mut replan);
    let file = fs::read_to_string(ctx.home.join("needs/n1.json")).unwrap();
    let parsed: Value = serde_json::from_str(&file).unwrap();
    assert_eq!(parsed["need"], "n1");
    assert_eq!(parsed["subject"], subject);
    assert_eq!(parsed["triage"], "");
    assert_eq!(parsed["thread"].as_str().unwrap().chars().count(), 40_000);
    let home = ctx.home.display().to_string();
    let want_prompt = format!(
        "Plan one need, as your system prompt describes. Its thread is in {home}/needs/n1.json. \
         Write {home}/plans/n1.json. Your final message is one line: handle, verdict, number of units."
    );
    assert_eq!(
        fs::read_to_string(ctx.home.join("board/n1.prompt")).unwrap(),
        want_prompt
    );
    assert!(host.spawned().iter().any(|cmd| cmd.contains("planner-run")));
    assert_eq!(st.board.planning.get("n1"), Some(&ctx.now()));
    let short_subject: String = subject.chars().take(70).collect();
    assert!(
        host.logs()
            .contains(&format!("board: planning new need n1 {short_subject}"))
    );
}

#[test]
fn operator_replied_after_sees_a_newer_operator_message_only() {
    let (_dir, ctx, host) = setup();
    let older = thread(
        "2026-10-14T09:00:00Z",
        "aaa111",
        "operator@bd371d96.local",
        "the ruling",
    );
    let newer = thread(
        "2026-10-16T09:00:00Z",
        "bbb222",
        "operator@bd371d96.local",
        "re-plan please",
    );
    host.reply("need show op1", 0, &format!("{older}\n{newer}"));
    assert!(operator_replied_after(
        &ctx,
        "op1",
        epoch("2026-10-15T00:00:00Z")
    ));
    assert!(!operator_replied_after(
        &ctx,
        "op1",
        epoch("2026-10-16T09:00:00Z")
    ));
    assert!(!operator_replied_after(
        &ctx,
        "op1",
        epoch("2027-01-01T00:00:00Z")
    ));
    let other = thread(
        "2030-01-01T00:00:00Z",
        "ccc333",
        "mind@bd371d96.local",
        "not the operator",
    );
    host.reply("need show op2", 0, &other);
    assert!(!operator_replied_after(&ctx, "op2", 0));
    host.reply(
        "need show op5",
        0,
        "From: operator@bd371d96.local\nno header first\n",
    );
    assert!(!operator_replied_after(&ctx, "op5", 0));
    let unparseable = thread(
        "2026-13-99T99:99:99Z",
        "ddd444",
        "operator@bd371d96.local",
        "bad time",
    );
    host.reply("need show op3", 0, &unparseable);
    assert!(!operator_replied_after(&ctx, "op3", 0));
    host.reply("need show op4", 1, "the mailspace is locked");
    assert!(!operator_replied_after(&ctx, "op4", 0));
}

#[test]
fn replan_answered_supersedes_the_plan_and_reopens_the_need_for_planning() {
    let (_dir, ctx, host) = setup();
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(ctx.home.join("plans/park1.json"), "{}").unwrap();
    host.reply(
        "need show park1",
        0,
        &thread(
            "2035-01-02T03:04:05Z",
            "beef01",
            "operator@bd371d96.local",
            "re-plan it",
        ),
    );
    let mut st = State::default();
    st.needs
        .insert(String::from("park1"), parked("hold", "waiting"));
    st.plans_seen = vec![String::from("park1.json"), String::from("other.json")];
    st.board.replied = vec![String::from("park1:planned")];
    let rows = vec![need_row("park1", "2020-01-01", "Old but answered")];
    let mut replan = replan_answered(&ctx, &mut st, &rows);
    assert_eq!(replan, BTreeSet::from([String::from("park1")]));
    assert_eq!(st.board.replan, vec![String::from("park1")]);
    assert_eq!(st.plans_seen, vec![String::from("other.json")]);
    assert!(!st.needs.contains_key("park1"));
    assert!(st.board.replied.is_empty());
    assert!(!ctx.home.join("plans/park1.json").exists());
    let superseded: Vec<String> = fs::read_dir(ctx.home.join("plans/superseded"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(superseded.len(), 1);
    assert!(
        superseded[0].starts_with("park1.")
            && Path::new(superseded[0].as_str())
                .extension()
                .is_some_and(|ext| ext == "json")
    );
    assert!(host.logs().contains(
        &"board: operator replied on parked need park1 (was hold); re-planning".to_string()
    ));
    let approved = BTreeSet::new();
    launch_planners(&ctx, &mut st, &rows, &approved, &mut replan);
    assert!(st.board.planning.contains_key("park1"));
    assert!(replan.is_empty() && st.board.replan.is_empty());
}

#[test]
fn a_parked_need_without_a_reply_is_left_alone() {
    let (_dir, ctx, host) = setup();
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(ctx.home.join("plans/park2.json"), "{}").unwrap();
    let ancient = thread(
        "2001-01-01T00:00:00Z",
        "cafe01",
        "operator@bd371d96.local",
        "ancient ruling",
    );
    let other = thread(
        "2035-01-01T00:00:00Z",
        "dead01",
        "mind@bd371d96.local",
        "not the operator",
    );
    host.reply("need show park2", 0, &format!("{ancient}\n{other}"));
    let mut st = State::default();
    st.needs
        .insert(String::from("park2"), parked("operator", "held"));
    st.board.replan = vec![String::from("elsewhere")];
    let rows = vec![need_row("park2", "2026-10-15", "Parked")];
    let replan = replan_answered(&ctx, &mut st, &rows);
    assert!(!replan.contains("park2"));
    assert_eq!(st.board.replan, vec![String::from("elsewhere")]);
    assert!(st.needs.contains_key("park2"));
    assert!(st.plans_seen.is_empty());
    assert!(ctx.home.join("plans/park2.json").exists());
    assert!(
        !host
            .logs()
            .iter()
            .any(|line| line.contains("operator replied"))
    );
}

#[test]
fn post_replies_sends_planned_once_and_merged_once() {
    let (_dir, ctx, host) = setup();
    host.reply("mail reply n1", 0, "accepted\nqueued on the server");
    let mut st = State::default();
    st.needs.insert(
        String::from("n1"),
        NeedRecord {
            verdict: String::from("split"),
            reason: String::from("two parts"),
            units: vec![String::from("u1"), String::from("u2")],
            revision: 1,
            ..NeedRecord::default()
        },
    );
    st.units.insert(
        String::from("u1"),
        merged_unit("Unit one", "abcdef1234567890"),
    );
    st.units.insert(
        String::from("u2"),
        merged_unit("Unit two", "abcdef1234567890"),
    );
    let rows = vec![need_row("n1", "2026-10-15", "Split me")];
    assert!(post_replies(&ctx, &mut st, &rows));
    let planned = fs::read_to_string(ctx.home.join("board/reply-n1-planned.txt")).unwrap();
    assert_eq!(
        planned,
        "pump: planned. verdict: split; revision 1; 2 unit(s).\ntwo parts\n- u1 Unit one\n- u2 Unit two\n"
    );
    let merged = fs::read_to_string(ctx.home.join("board/reply-n1-merged.txt")).unwrap();
    assert_eq!(
        merged,
        "pump: every unit of this need is merged into branch next (r1).\n- u1 merged abcdef1234\n- u2 merged abcdef1234\nNot reviewed. Not on main.\n"
    );
    let mails = || {
        host.commands()
            .iter()
            .filter(|cmd| cmd.contains("mail reply n1"))
            .count()
    };
    assert_eq!(mails(), 2);
    assert!(host.commands().iter().any(|cmd| {
        cmd.contains("--from mind")
            && cmd.contains("--to operator@bd371d96.local")
            && cmd.contains("reply-n1-planned.txt")
    }));
    assert!(
        host.logs()
            .contains(&"board reply n1 planned rc=0 queued on the server".to_string())
    );
    assert_eq!(
        st.board.replied,
        vec![String::from("n1:planned"), String::from("n1:merged")]
    );
    assert!(!post_replies(&ctx, &mut st, &rows));
    assert_eq!(mails(), 2);
}

#[test]
fn the_merged_reply_waits_until_every_unit_is_satisfied() {
    let (_dir, ctx, host) = setup();
    host.reply("mail reply n2", 0, "queued");
    let mut st = State::default();
    st.needs.insert(
        String::from("n2"),
        NeedRecord {
            verdict: String::from("plan"),
            reason: String::from("one first"),
            units: vec![String::from("u1"), String::from("u2")],
            revision: 1,
            ..NeedRecord::default()
        },
    );
    st.units
        .insert(String::from("u1"), merged_unit("One", "1111111111aaaa"));
    st.units.insert(String::from("u2"), Unit::default());
    let rows = vec![need_row("n2", "2026-10-15", "Two units")];
    assert!(post_replies(&ctx, &mut st, &rows));
    assert_eq!(st.board.replied, vec![String::from("n2:planned")]);
    assert!(!ctx.home.join("board/reply-n2-merged.txt").exists());
    let unit = st.units.get_mut("u2").unwrap();
    unit.status = Status::Merged;
    unit.commit = Some(String::from("2222222222bbbb"));
    assert!(post_replies(&ctx, &mut st, &rows));
    let merged = fs::read_to_string(ctx.home.join("board/reply-n2-merged.txt")).unwrap();
    assert!(merged.contains("merged into branch next (r1)."));
    assert!(merged.contains("- u2 merged 2222222222"));
    assert_eq!(
        st.board.replied,
        vec![String::from("n2:planned"), String::from("n2:merged")]
    );
}

#[test]
fn a_failed_reply_is_retried_on_the_next_pass() {
    let (_dir, ctx, host) = setup();
    host.reply("mail reply f1", 1, "relay down\ncome back later");
    let mut st = State::default();
    st.needs
        .insert(String::from("f1"), parked("single", "one unit"));
    let rows = vec![need_row("f1", "2026-10-15", "One")];
    assert!(!post_replies(&ctx, &mut st, &rows));
    assert!(st.board.replied.is_empty());
    assert!(
        host.logs()
            .contains(&"board reply f1 planned rc=1 come back later".to_string())
    );
    host.reply("mail reply f1", 0, "queued");
    assert!(post_replies(&ctx, &mut st, &rows));
    assert_eq!(st.board.replied, vec![String::from("f1:planned")]);
}

#[test]
fn board_skips_a_pass_inside_the_every_window_and_pulls_then_plans_otherwise() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.board.at = ctx.now();
    board(&ctx, &mut st);
    assert!(host.commands().is_empty());
    host.reply("sync pull", 0, "pulled 3 messages");
    host.reply(
        "need list --from operator@bd371d96.local",
        0,
        &list(&[row("n9", "2026-10-15T00:00", "Plan me", &[])]),
    );
    host.reply("--held", 0, "[]");
    host.reply("--json --to mind", 0, "[]");
    host.reply(
        "need show n9",
        0,
        &thread(
            "2026-10-15T00:00:00Z",
            "f00d01",
            "operator@bd371d96.local",
            "the need",
        ),
    );
    host.advance(301);
    board(&ctx, &mut st);
    assert_eq!(st.board.at, ctx.now());
    assert!(
        host.logs()
            .contains(&"sync pull rc=0 pulled 3 messages".to_string())
    );
    assert!(st.board.planning.contains_key("n9"));
    assert!(host.spawned().iter().any(|cmd| cmd.contains("planner-run")));
    assert!(!host.spawned().iter().any(|cmd| cmd.contains("sync push")));
}

#[test]
fn a_planner_started_in_the_future_is_kept_and_does_not_underflow() {
    let (_dir, ctx, _host) = setup();
    let mut st = State::default();
    let ahead = ctx.now() + 5_000;
    st.board.planning.insert(String::from("ahead"), ahead);
    expire_planning(&ctx, &mut st);
    assert_eq!(st.board.planning.get("ahead"), Some(&ahead));
    assert!(!exceptions(&ctx).contains("plan-no-answer"));
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

#[test]
fn post_replies_sends_one_revised_stage_per_revision() {
    let (_dir, ctx, host) = setup();
    host.reply("mail reply n3", 0, "queued");
    let mut st = State::default();
    st.needs.insert(
        String::from("n3"),
        NeedRecord {
            verdict: String::from("units"),
            reason: String::from("revised"),
            units: vec![String::from("u1")],
            revision: 2,
            ..NeedRecord::default()
        },
    );
    st.units.insert(
        String::from("u1"),
        merged_unit("Unit one", "abcdef1234567890"),
    );
    let rows = vec![need_row("n3", "2026-10-15", "Revised")];
    assert!(post_replies(&ctx, &mut st, &rows));
    let revised = fs::read_to_string(ctx.home.join("board/reply-n3-revised-2.txt")).unwrap();
    assert!(revised.contains("pump: revised to r2."), "{revised}");
    assert!(st.board.replied.contains(&String::from("n3:revised-2")));
}
