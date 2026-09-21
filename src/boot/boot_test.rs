//! Unit tests for the boot digest's pure helpers.

use chrono::{TimeZone, Utc};

use super::format::{age_label, first_paragraph, one_line};
use super::goals::{bucket, cap_chars, completion_claim, normalize_status, read_goal};
use super::handles::{HandleRow, Verdict, apply_verdicts, parse_verdict, triage_slices};
use super::probe::{ProbeVerdict, parse_output};
use super::truncate::{Truncation, capped, record};
use super::{GoalRow, SeatRow, goal_line, human_seconds, seat_line};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 21, 12, 0, 0)
        .single()
        .unwrap_or_default()
}

fn row(handle: &str, kind: &str, to: &str) -> HandleRow {
    HandleRow::new(
        handle,
        kind,
        "2026-09-19T12:00:00+00:00",
        "hand@x.local",
        to,
        "subject",
        now(),
    )
}

#[test]
fn age_label_reads_hours_and_days() {
    assert_eq!(age_label("2026-09-21T11:00:00+00:00", now()), "1h");
    assert_eq!(age_label("2026-09-19T12:00:00+00:00", now()), "2d");
    assert_eq!(age_label("2026-09-21T11:50:00+00:00", now()), "10m");
    assert_eq!(age_label("not-a-date", now()), "?");
}

#[test]
fn human_seconds_scales() {
    assert_eq!(human_seconds(600), "10m");
    assert_eq!(human_seconds(7_200), "2h");
    assert_eq!(human_seconds(172_800), "2d");
}

#[test]
fn first_paragraph_stops_at_a_blank_line() {
    let text = "\n# Title\n\nbody line\nsecond line\n\nlater\n";
    assert_eq!(first_paragraph(text, 6), vec!["# Title".to_string()]);
}

#[test]
fn first_paragraph_caps_lines() {
    let text = "a\nb\nc\nd\ne\nf\ng\n";
    assert_eq!(first_paragraph(text, 3).len(), 3);
}

#[test]
fn one_line_collapses_whitespace() {
    assert_eq!(one_line("a  b\n c", 40), "a b c");
    assert!(one_line(&"x".repeat(200), 20).ends_with('…'));
}

#[test]
fn cap_chars_is_char_boundary_safe() {
    let text = "αβγδεζη";
    let capped = cap_chars(text, 4);
    assert_eq!(capped.chars().count(), 4);
}

#[test]
fn truncation_records_only_when_it_bits() {
    assert!(Truncation::of("s", 5, 5).is_none());
    let record_of = Truncation::of("s", 5, 9);
    assert_eq!(record_of.map(|t| (t.shown, t.total)), Some((5, 9)));
}

#[test]
fn capped_keeps_the_head_and_reports() {
    let mut drops = Vec::new();
    let kept = capped(vec![1, 2, 3, 4], 2, "nums", &mut drops);
    assert_eq!(kept, vec![1, 2]);
    assert_eq!(drops.len(), 1);
    assert_eq!(drops[0].section, "nums");

    let mut none = Vec::new();
    let all = capped(vec![1, 2], 2, "nums", &mut none);
    assert_eq!(all, vec![1, 2]);
    assert!(none.is_empty());
}

#[test]
fn record_ignores_a_cap_that_did_not_bite() {
    let mut drops = Vec::new();
    record(&mut drops, "s", 3, 3);
    assert!(drops.is_empty());
}

#[test]
fn verdict_slugs_are_stable() {
    assert_eq!(Verdict::Stale.slug(), "stale");
    assert_eq!(Verdict::Unverified.slug(), "unverified");
    assert_eq!(parse_verdict("STALE"), Some(Verdict::Stale));
    assert_eq!(parse_verdict("nonsense"), None);
}

#[test]
fn probe_verdicts_match_full_handle_and_unique_prefix() {
    let mut rows = vec![row("abcdef01", "need", "mind@x.local")];
    let verdicts = vec![ProbeVerdict {
        handle: "abcd".to_string(),
        verdict: "stale".to_string(),
        detail: Some("commit on main".to_string()),
    }];
    apply_verdicts(&mut rows, &verdicts, &[]);
    assert_eq!(rows[0].verdict, Verdict::Stale);
    assert_eq!(rows[0].detail.as_deref(), Some("commit on main"));
}

#[test]
fn short_prefixes_do_not_match() {
    let mut rows = vec![row("abcdef01", "need", "mind@x.local")];
    let verdicts = vec![ProbeVerdict {
        handle: "abc".to_string(),
        verdict: "stale".to_string(),
        detail: None,
    }];
    apply_verdicts(&mut rows, &verdicts, &[]);
    assert_eq!(rows[0].verdict, Verdict::Open);
}

#[test]
fn graph_blockers_outrank_probe_verdicts() {
    let mut rows = vec![row("abcdef01", "need", "mind@x.local")];
    let verdicts = vec![ProbeVerdict {
        handle: "abcdef01".to_string(),
        verdict: "stale".to_string(),
        detail: None,
    }];
    apply_verdicts(&mut rows, &verdicts, &["abcdef01".to_string()]);
    assert_eq!(rows[0].verdict, Verdict::Blocked);
}

#[test]
fn triage_slices_only_open_needs_and_wants() {
    let mut rows = Vec::new();
    for index in 0..14 {
        rows.push(row(&format!("need{index:04}"), "need", "mind@x.local"));
    }
    rows.push(row("task0001", "task", "hand@x.local"));
    let mut stale = row("need9999", "need", "mind@x.local");
    stale.verdict = Verdict::Stale;
    rows.push(stale);

    let slices = triage_slices(&rows);
    assert_eq!(slices.len(), 2);
    assert_eq!(slices[0].size, 12);
    assert_eq!(slices[1].size, 2);
    assert!(slices.iter().all(|slice| slice.handles.len() == slice.size));
}

#[test]
fn probe_output_parses_the_documented_contract() {
    let json = r#"{
      "facts": ["radix main 0ec4d00a7"],
      "sections": [{"title": "world", "lines": ["line one"]}],
      "verdicts": [{"handle": "abcdef01", "verdict": "stale"}]
    }"#;
    let parsed = parse_output(json).unwrap_or_default();
    assert_eq!(parsed.facts.len(), 1);
    assert_eq!(parsed.sections[0].title, "world");
    assert_eq!(parsed.verdicts[0].handle, "abcdef01");
}

#[test]
fn empty_probe_output_is_a_clean_contribution() {
    assert_eq!(parse_output("   ").map(|o| o.sections.len()), Ok(0));
    assert!(parse_output("not json").is_err());
}

#[test]
fn seat_line_names_the_verdict() {
    let seat = SeatRow {
        role: "hand".to_string(),
        kind: "hand".to_string(),
        harness: Some("subagent".to_string()),
        binding: "subagent harness, unbound".to_string(),
        verdict: Verdict::Unverified,
    };
    let line = seat_line(&seat);
    assert!(line.contains("hand"));
    assert!(line.contains("unverified"));
}

#[test]
fn goal_line_falls_back_when_status_is_absent() {
    let goal = GoalRow {
        handle: "gol_1".to_string(),
        path: "docs/factory/x/goal.md".to_string(),
        bucket: None,
        status_text: None,
        register: None,
        claim_mismatch: None,
        frontier: Vec::new(),
    };
    assert!(goal_line(&goal).contains("(none)"));
}

#[test]
fn status_bucket_is_the_leading_word() {
    assert_eq!(bucket("active — 7/26 delivered"), "active");
    assert_eq!(bucket("planned pre-implementation"), "planned");
    assert_eq!(bucket("done."), "done");
}

#[test]
fn completion_claim_reads_the_numerator_and_denominator() {
    assert_eq!(completion_claim("active — 7/26 delivered"), Some((7, 26)));
    assert_eq!(completion_claim("active — 24/26 delivered"), Some((24, 26)));
    assert_eq!(completion_claim("done — 137/200 delivered"), Some((137, 200)));
    assert_eq!(completion_claim("no numbers here"), None);
}

#[test]
fn status_slugs_absorb_multi_word_phrases() {
    assert_eq!(normalize_status("**in flight**"), "in flight");
    assert_eq!(normalize_status("in-flight"), "in flight");
    assert_eq!(normalize_status("`pending`"), "pending");
    assert_eq!(normalize_status("deferred (operator)"), "deferred");
}

#[test]
fn read_goal_detects_a_status_line_that_contradicts_its_register() {
    let text = "\
# GOAL: sample

**Status**: active — 7/26 delivered

## Ledger

| Unit | Need | Status | Notes |
| --- | --- | --- | --- |
| ON-R1 | `a1` | done | landed |
| ON-R2 | `a2` | done | landed |
| ON-R3 | `a3` | pending | next |
| ON-R4 | `a4` | in flight | held |
";
    let read = read_goal(text);
    assert_eq!(read.bucket.as_deref(), Some("active"));
    assert_eq!(read.claim, Some((7, 26)));
    let tally = read.register.as_ref();
    assert_eq!(tally.map(|t| t.rows), Some(4));
    assert_eq!(tally.map(|t| t.count_of("done")), Some(2));
    assert!(read.claim_mismatch);
    assert_eq!(read.frontier.len(), 2);
    assert_eq!(read.frontier[0].unit, "ON-R3");
    assert_eq!(read.frontier[1].unit, "ON-R4");
}

#[test]
fn read_goal_is_quiet_when_the_claim_matches_the_register() {
    let text = "\
**Status**: active — 1/2 delivered

| Unit | Status |
| --- | --- |
| U1 | done |
| U2 | pending |
";
    let read = read_goal(text);
    assert!(!read.claim_mismatch);
    assert_eq!(read.frontier.len(), 1);
}

#[test]
fn read_goal_without_a_register_reports_nothing() {
    let read = read_goal("**Status**: planned\n\nno tables here\n");
    assert_eq!(read.bucket.as_deref(), Some("planned"));
    assert!(read.register.is_none());
    assert!(read.frontier.is_empty());
    assert!(!read.claim_mismatch);
}

#[test]
fn read_goal_ignores_tables_without_a_status_column() {
    let text = "\
**Status**: active

| Name | Owner |
| --- | --- |
| a | b |
";
    let read = read_goal(text);
    assert!(read.register.is_none());
}

#[test]
fn escaped_pipes_do_not_split_a_cell() {
    let text = "\
**Status**: active

| Unit | Status | Note |
| --- | --- | --- |
| U1 | done | a \\| b |
| U2 | pending | c |
";
    let read = read_goal(text);
    assert_eq!(read.register.as_ref().map(|t| t.rows), Some(2));
    assert_eq!(read.frontier.len(), 1);
}
