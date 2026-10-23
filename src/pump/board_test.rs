//! Tests for [`crate::pump::board`]: candidate need rows (needs with an open
//! pump record), the
//! skip list, planner expiry and planner launch, driven through
//! [`FakeHost`] replies.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::pump::board::*;
use crate::pump::config::{BoardConfig, PumpConfig};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::record::send_reply;
use crate::pump::state::{NeedRecord, State, Status, Unit};

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
        branch: String::from("next"),
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

const NEED_BASE: &str = "1111111111111111111111111111111111111111";
const NEED_TIP: &str = "2222222222222222222222222222222222222222";
const LANDING_CANDIDATE: &str = "3333333333333333333333333333333333333333";
const OBSERVED_TARGET: &str = "4444444444444444444444444444444444444444";

fn managed_need_with_settled_units() -> State {
    let mut st = State::default();
    st.needs.insert(
        String::from("n-managed"),
        NeedRecord {
            verdict: String::from("plan"),
            reason: String::from("one unit"),
            units: vec![String::from("u-managed")],
            revision: 1,
            ..NeedRecord::default()
        },
    );
    st.units.insert(
        String::from("u-managed"),
        Unit {
            need: String::from("n-managed"),
            target: String::from("default"),
            revision: 1,
            status: Status::Merged,
            commit: Some(String::from(NEED_TIP)),
            ..Unit::default()
        },
    );
    crate::pump::need::reserve(&mut st, "default", "n-managed", 1, NEED_BASE).unwrap();
    crate::pump::need::record_create_job(&mut st, "default", "n-managed", "init-managed").unwrap();
    crate::pump::need::record_created(
        &mut st,
        "default",
        "n-managed",
        "init-managed",
        NEED_BASE,
        NEED_BASE,
    )
    .unwrap();
    crate::pump::need::record_tip(&mut st, "default", "n-managed", 1, NEED_BASE, NEED_TIP).unwrap();
    crate::pump::need::freeze(&mut st, "default", "n-managed", 1, NEED_TIP).unwrap();
    st
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

/// One JSON record as `vivi pump list --json` prints it.
fn record(handle: &str, need: &str) -> String {
    format!(
        "{{\"handle\": \"{handle}\", \"kind\": \"pump\", \"need\": \"{need}\", \"status\": \"open\"}}"
    )
}

/// Script `pump list` to hold open records for `needs`.
fn open_records(host: &FakeHost, needs: &[&str]) {
    let records: Vec<String> = needs
        .iter()
        .map(|need| record(&format!("p-{need}"), need))
        .collect();
    host.reply("pump list --status open --json", 0, &list(&records));
}

/// Script the lookup of `need`'s open pump record to `record_handle`, so a
/// progress reply is posted with `mail reply <record_handle>`.
fn reply_record(host: &FakeHost, need: &str, record_handle: &str) {
    host.reply(
        &format!("pump list --status open --json --need {need}"),
        0,
        &list(&[record(record_handle, need)]),
    );
}

/// The one `mail reply` command the fake host ran.
fn mail_reply(host: &FakeHost) -> String {
    host.commands()
        .into_iter()
        .find(|cmd| cmd.contains("mail reply"))
        .expect("a mail reply ran")
}

fn handles(rows: &[NeedRow]) -> Vec<&str> {
    rows.iter().map(|row| row.handle.as_str()).collect()
}

#[test]
fn a_need_with_an_open_pump_record_is_a_candidate() {
    let (_dir, ctx, host) = setup();
    open_records(&host, &["a", "d"]);
    host.reply(
        "--json --to mind",
        0,
        &list(&[
            row("a", "2026-10-15", "A", &[]),
            row("d", "2026-10-15", "D", &[]),
        ]),
    );
    host.reply("--held", 0, "[]");
    let rows = candidate_rows(&ctx);
    assert_eq!(handles(&rows), vec!["a", "d"]);
    assert_eq!(rows[0].subject, "A");
}

#[test]
fn a_marked_need_without_a_pump_record_is_not_a_candidate() {
    let (_dir, ctx, host) = setup();
    open_records(&host, &["has-record"]);
    host.reply(
        "--json --to mind",
        0,
        &list(&[
            row("marked", "2026-10-15", "Marked, no record", &["pump"]),
            row("has-record", "2026-10-15", "Has a record", &[]),
        ]),
    );
    host.reply(
        "--held",
        0,
        &list(&[row("held-marked", "2026-10-15", "Held", &["pump"])]),
    );
    let rows = candidate_rows(&ctx);
    assert_eq!(handles(&rows), vec!["has-record"]);
}

#[test]
fn a_configured_sender_no_longer_makes_its_needs_candidates() {
    let (_dir, ctx, host) = setup();
    open_records(&host, &[]);
    host.reply(
        "need list --from operator@bd371d96.local",
        0,
        &list(&[row("a", "2026-10-15", "A", &[])]),
    );
    host.reply(
        "--json --to mind",
        0,
        &list(&[row("a", "2026-10-15", "A", &[])]),
    );
    assert!(candidate_rows(&ctx).is_empty());
    assert!(
        host.commands()
            .iter()
            .all(|cmd| !cmd.contains("need list --from")),
        "no sender query is issued: {:?}",
        host.commands()
    );
}

#[test]
fn candidates_come_from_held_and_addressed_lists_and_are_deduplicated() {
    let (_dir, ctx, host) = setup();
    open_records(&host, &["b", "c", "d"]);
    host.reply(
        "--held",
        0,
        &list(&[
            row("c", "2026-10-15", "C", &[]),
            row("b", "2026-10-15", "B", &[]),
        ]),
    );
    host.reply(
        "--json --to mind",
        0,
        &list(&[
            row("d", "2026-10-15", "D", &[]),
            row("b", "2026-10-15", "B again", &[]),
        ]),
    );
    let rows = candidate_rows(&ctx);
    assert_eq!(handles(&rows), vec!["c", "b", "d"]);
    assert_eq!(rows[1].subject, "B");
}

#[test]
fn a_record_whose_need_is_not_addressed_to_the_pump_gives_no_rows_and_no_log() {
    let (_dir, ctx, host) = setup();
    open_records(&host, &["elsewhere"]);
    host.reply("--held", 0, "[]");
    host.reply("--json --to mind", 0, "[]");
    assert!(candidate_rows(&ctx).is_empty());
    let logs = host.logs().join("\n");
    assert!(!logs.contains("not planned"), "logs: {logs}");
}

#[test]
fn an_unreadable_record_list_plans_nothing() {
    let (_dir, ctx, host) = setup();
    host.reply("pump list --status open --json", 1, "error: locked");
    host.reply(
        "--json --to mind",
        0,
        &list(&[row("a", "2026-10-15", "A", &[])]),
    );
    assert!(candidate_rows(&ctx).is_empty());
    let logs = host.logs().join("\n");
    assert!(logs.contains("pump record list unreadable"), "logs: {logs}");
}

#[test]
fn an_unreadable_need_list_keeps_what_the_other_list_gave() {
    let (_dir, ctx, host) = setup();
    open_records(&host, &["n3"]);
    host.reply("--held", 1, "error: the mailspace is locked\nsecond line");
    host.reply(
        "--json --to mind",
        0,
        &list(&[row("n3", "2026-10-15", "Three", &[])]),
    );
    let rows = candidate_rows(&ctx);
    assert_eq!(handles(&rows), vec!["n3"]);
    let logs = host.logs().join("\n");
    assert!(
        logs.contains(
            "board list unreadable (need list --status open --json --held): \
             error: the mailspace is locked | second line"
        ),
        "logs: {logs}"
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
fn replied_after_sees_a_newer_message_from_anyone_but_the_pump() {
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
    assert!(replied_after(&ctx, "op1", epoch("2026-10-15T00:00:00Z")));
    assert!(!replied_after(&ctx, "op1", epoch("2026-10-16T09:00:00Z")));
    assert!(!replied_after(&ctx, "op1", epoch("2027-01-01T00:00:00Z")));
    let other = thread(
        "2030-01-01T00:00:00Z",
        "ccc333",
        "mind@bd371d96.local",
        "the pump's own reply",
    );
    host.reply("need show op2", 0, &other);
    assert!(!replied_after(&ctx, "op2", 0));
    let cto = thread(
        "2030-01-01T00:00:00Z",
        "eee555",
        "cto@bd371d96.local",
        "approved: plan units",
    );
    host.reply("need show op6", 0, &cto);
    assert!(replied_after(&ctx, "op6", 0));
    host.reply(
        "need show op5",
        0,
        "From: operator@bd371d96.local\nno header first\n",
    );
    assert!(!replied_after(&ctx, "op5", 0));
    let unparseable = thread(
        "2026-13-99T99:99:99Z",
        "ddd444",
        "operator@bd371d96.local",
        "bad time",
    );
    host.reply("need show op3", 0, &unparseable);
    assert!(!replied_after(&ctx, "op3", 0));
    host.reply("need show op4", 1, "the mailspace is locked");
    assert!(!replied_after(&ctx, "op4", 0));
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
    replan_answered(&ctx, &mut st, &rows);
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
    let rows = vec![need_row("park2", "2026-10-15", "Parked")];
    replan_answered(&ctx, &mut st, &rows);
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
    reply_record(&host, "n1", "p1");
    host.reply("mail reply p1", 0, "accepted\nqueued on the server");
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
        "pump: planned. verdict: split; revision 1; 2 unit(s).\ntwo parts\n- u1 [default] Unit one\n- u2 [default] Unit two\n"
    );
    let merged = fs::read_to_string(ctx.home.join("board/reply-n1-merged.txt")).unwrap();
    assert_eq!(
        merged,
        "pump: every unit of this need is merged into its target integration branch (r1).\n- u1 [default] merged abcdef1234\n- u2 [default] merged abcdef1234\nNot reviewed. Not on main.\n"
    );
    let mails = || {
        host.commands()
            .iter()
            .filter(|cmd| cmd.contains("mail reply p1"))
            .count()
    };
    assert_eq!(mails(), 2);
    assert!(host.commands().iter().any(|cmd| {
        cmd.contains("mail reply p1 --from mind")
            && cmd.contains("--to operator@bd371d96.local")
            && cmd.contains("reply-n1-planned.txt")
    }));
    assert!(
        host.commands()
            .iter()
            .all(|cmd| !cmd.contains("mail reply n1")),
        "nothing is posted on the need thread: {:?}",
        host.commands()
    );
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
fn managed_need_reports_implementation_before_landing_and_acceptance_after_cleanup() {
    let (_dir, ctx, host) = setup();
    reply_record(&host, "n-managed", "p-managed");
    host.reply("mail reply p-managed", 0, "queued");
    let mut st = managed_need_with_settled_units();
    let rows = vec![need_row("n-managed", "2026-10-15", "Managed need")];

    assert!(post_replies(&ctx, &mut st, &rows));
    let implemented =
        fs::read_to_string(ctx.home.join("board/reply-n-managed-implemented.txt")).unwrap();
    assert!(implemented.contains("settled on this managed need's isolated branch"));
    assert!(implemented.contains("not yet fully verified"));
    assert!(!implemented.contains("Not reviewed"));
    assert!(!ctx.home.join("board/reply-n-managed-merged.txt").exists());
    assert_eq!(
        st.board.replied,
        vec![
            String::from("n-managed:planned"),
            String::from("n-managed:implemented-r1")
        ]
    );

    let binding = crate::pump::need::binding_for(&st, "default", "n-managed")
        .unwrap()
        .unwrap();
    crate::pump::need::reserve_landing(&mut st, "default", "n-managed", &binding, "land-managed")
        .unwrap();
    crate::pump::need::record_landing(
        &mut st,
        "default",
        "n-managed",
        "land-managed",
        crate::pump::need::NeedLandingReceipt {
            binding,
            acceptance_receipt_key: String::from("accept-managed-r1"),
            acceptance_job_id: String::from("audit-managed-r1"),
            verdict: String::from("clean_pass"),
            landing_job_id: String::from("land-managed"),
            target_ref: String::from("refs/heads/next"),
            candidate_sha: String::from(LANDING_CANDIDATE),
            runner_published_sha: String::from(LANDING_CANDIDATE),
            observed_target_sha: String::from(OBSERVED_TARGET),
            accepted_tip_in_candidate: true,
            candidate_in_observed_target: true,
            merge_report: String::from("verified merge"),
        },
    )
    .unwrap();
    crate::pump::need::record_cleanup_job(&mut st, "default", "n-managed", "cleanup-managed")
        .unwrap();
    crate::pump::need::finish_cleanup(
        &mut st,
        "default",
        "n-managed",
        "cleanup-managed",
        crate::pump::need::NeedCleanupReceipt {
            cleanup_job_id: String::from("cleanup-managed"),
            branch_ref: String::from("refs/heads/vivi/need/n-managed"),
            expected_sha: String::from(NEED_TIP),
            runner_state: String::from("deleted"),
            before_sha: Some(String::from(NEED_TIP)),
            verified_absent: true,
            completed_at: 10,
        },
    )
    .unwrap();

    assert!(post_replies(&ctx, &mut st, &rows));
    let merged = fs::read_to_string(ctx.home.join("board/reply-n-managed-merged.txt")).unwrap();
    assert!(merged.contains("accepted and verified on its target integration ref(s)"));
    assert!(merged.contains("accepted clean_pass (accept-managed-r1)"));
    assert!(merged.contains("refs/heads/next observed at 4444444444"));
    assert!(merged.contains("Promotion to main is separate."));
    assert!(!merged.contains("Not reviewed"));
    assert_eq!(
        st.board.replied,
        vec![
            String::from("n-managed:planned"),
            String::from("n-managed:implemented-r1"),
            String::from("n-managed:managed-merged-r1")
        ]
    );
    assert!(!post_replies(&ctx, &mut st, &rows));
}

#[test]
fn send_reply_omits_an_empty_recipient_and_quotes_role_identities() {
    let board = BoardConfig {
        from: Vec::new(),
        ..BoardConfig::default()
    };
    let cfg = PumpConfig {
        board: board.clone(),
        branch: String::from("next"),
        reply_from: String::from("mind role"),
        ..PumpConfig::default()
    };
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg);
    reply_record(&host, "empty-to", "p-empty");
    host.reply("mail reply p-empty", 0, "queued");
    assert!(send_reply(&ctx, "empty-to", "planned", "body"));
    let command = mail_reply(&host);
    assert!(command.contains("--from 'mind role'"), "{command}");
    assert!(!command.contains("--to "), "{command}");

    let cfg = PumpConfig {
        board: BoardConfig {
            reply_to: Some(String::from("operator role")),
            ..board
        },
        branch: String::from("next"),
        reply_from: String::from("mind role"),
        ..PumpConfig::default()
    };
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg);
    reply_record(&host, "quoted-to", "p-quoted");
    host.reply("mail reply p-quoted", 0, "queued");
    assert!(send_reply(&ctx, "quoted-to", "planned", "body"));
    let command = mail_reply(&host);
    assert!(
        command.contains("--from 'mind role' --to 'operator role'"),
        "{command}"
    );
}

#[test]
fn the_merged_reply_waits_until_every_unit_is_satisfied() {
    let (_dir, ctx, host) = setup();
    reply_record(&host, "n2", "p2");
    host.reply("mail reply p2", 0, "queued");
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
    assert!(merged.contains("merged into its target integration branch (r1)."));
    assert!(merged.contains("- u2 [default] merged 2222222222"));
    assert_eq!(
        st.board.replied,
        vec![String::from("n2:planned"), String::from("n2:merged")]
    );
}

#[test]
fn the_merged_reply_carries_the_timing_summary() {
    let (_dir, ctx, host) = setup();
    reply_record(&host, "n2", "p2");
    host.reply("mail reply p2", 0, "queued");
    host.reply(
        "need attempts n2",
        0,
        concat!(
            "{\"need\":\"n2\",\"opened\":910000,\"attempts\":[",
            "{\"attempt\":\"n2-1-a1\",\"outcome\":\"delivered\",\"agent_seconds\":60},",
            "{\"attempt\":\"n2-1-a2\",\"outcome\":\"delivered\",\"agent_seconds\":3600}]}",
        ),
    );
    let mut st = State::default();
    st.needs.insert(
        String::from("n2"),
        NeedRecord {
            verdict: String::from("units"),
            reason: String::from("one unit"),
            units: vec![String::from("u1")],
            revision: 1,
            ..NeedRecord::default()
        },
    );
    st.units
        .insert(String::from("u1"), merged_unit("One", "1111111111aaaa"));
    let rows = vec![need_row("n2", "2026-10-15", "One unit")];
    assert!(post_replies(&ctx, &mut st, &rows));
    let merged = fs::read_to_string(ctx.home.join("board/reply-n2-merged.txt")).unwrap();
    assert!(
        merged.contains(
            "timing: open-to-done 1d1h; attempts 2; agent time 1h1m; slowest n2-1-a2 (1h)"
        ),
        "merged reply was {merged:?}"
    );
    assert_eq!(
        st.board.replied,
        vec![String::from("n2:planned"), String::from("n2:merged")]
    );
}

#[test]
fn a_failed_reply_is_retried_on_the_next_pass() {
    let (_dir, ctx, host) = setup();
    reply_record(&host, "f1", "pf1");
    host.reply("mail reply pf1", 1, "relay down\ncome back later");
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
    host.reply("mail reply pf1", 0, "queued");
    assert!(post_replies(&ctx, &mut st, &rows));
    assert_eq!(st.board.replied, vec![String::from("f1:planned")]);
}

#[test]
fn board_skips_a_pass_inside_the_every_window_and_pulls_otherwise() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.board.at = ctx.now();
    board(&ctx, &mut st);
    assert!(host.commands().is_empty());
    host.reply("sync pull", 0, "pulled 3 messages");
    open_records(&host, &["n9"]);
    host.reply("--held", 0, "[]");
    host.reply(
        "--json --to mind",
        0,
        &list(&[row("n9", "2026-10-15T00:00", "Plan me", &[])]),
    );
    host.advance(301);
    board(&ctx, &mut st);
    assert_eq!(st.board.at, ctx.now());
    assert!(
        host.logs()
            .contains(&"sync pull rc=0 pulled 3 messages".to_string())
    );
    assert!(st.board.planning.is_empty());
    assert!(st.jobs.is_empty(), "the board pass plans nothing");
    assert!(!host.spawned().iter().any(|cmd| cmd.contains("sync push")));
}

#[test]
fn a_slow_board_pass_waits_the_interval_from_completion() {
    let (_dir, ctx, host) = setup_with(BoardConfig {
        from: vec![String::from("operator@bd371d96.local")],
        every: 60,
        ..BoardConfig::default()
    });
    host.advance_on_run(30);
    host.reply("need list", 0, "[]");
    let mut st = State::default();

    board(&ctx, &mut st);

    let command_count = host.commands().len();
    assert!(ctx.now().saturating_sub(st.board.at) < ctx.cfg.board.every);
    board(&ctx, &mut st);
    assert_eq!(host.commands().len(), command_count);
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
    reply_record(&host, "n3", "p3");
    host.reply("mail reply p3", 0, "queued");
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

#[test]
fn a_plan_verdict_that_waits_for_a_ruling_is_posted_on_the_need_thread() {
    let (_dir, ctx, host) = setup();
    reply_record(&host, "n4", "p4");
    host.reply("mail reply", 0, "queued");
    let mut st = State::default();
    st.needs
        .insert(String::from("n4"), parked("operator", "which database?"));
    let rows = vec![need_row("n4", "2026-10-15", "Ask")];
    assert!(post_replies(&ctx, &mut st, &rows));
    let commands = host.commands();
    assert!(
        commands
            .iter()
            .any(|cmd| cmd.contains("mail reply n4 --from mind")),
        "the question the operator answers stays on the need thread: {commands:?}"
    );
    assert!(
        commands.iter().all(|cmd| !cmd.contains("mail reply p4")),
        "and is not duplicated on the record: {commands:?}"
    );
    assert_eq!(st.board.replied, vec![String::from("n4:planned")]);
}

#[test]
fn a_progress_reply_for_a_need_without_an_open_record_is_dropped_and_retried() {
    let (_dir, ctx, host) = setup();
    host.reply("pump list --status open --json --need n5", 0, "[]");
    let mut st = State::default();
    st.needs
        .insert(String::from("n5"), parked("single", "one unit"));
    let rows = vec![need_row("n5", "2026-10-15", "No record")];
    assert!(!post_replies(&ctx, &mut st, &rows));
    assert!(st.board.replied.is_empty(), "retried on a later pass");
    assert!(
        host.commands()
            .iter()
            .all(|cmd| !cmd.contains("mail reply")),
        "nothing is posted anywhere: {:?}",
        host.commands()
    );
    assert!(
        host.logs()
            .contains(&"board reply n5 planned dropped: no open pump record".to_string())
    );
}

#[test]
fn the_record_of_a_pump_created_need_is_neither_planned_nor_reported() {
    let (_dir, ctx, host) = setup();
    open_records(&host, &["a", "gatefix"]);
    host.reply(
        "--json --to mind",
        0,
        &list(&[row("a", "2026-10-15", "A", &[])]),
    );
    host.reply("--held", 0, "[]");
    let own = std::collections::BTreeSet::from([String::from("gatefix")]);

    let rows = candidate_rows_except(&ctx, &own);

    assert_eq!(handles(&rows), vec!["a"]);
    assert!(
        host.logs().iter().all(|line| !line.contains("gatefix")),
        "{:?}",
        host.logs()
    );
}
