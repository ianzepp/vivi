//! What `vivi mind status` and `vivi mind probe` print.

use chrono::{FixedOffset, TimeZone};

use super::decision_lines;
use super::status_lines;
use crate::mind::probe::DecisionDetail;
use crate::mind::socket::StatusView;
use crate::mind::wake::{WakeRecord, readable_time_in};

fn view(last_wake: Option<WakeRecord>) -> StatusView {
    StatusView {
        authority: "observe".into(),
        paused: false,
        busy: false,
        session_id: None,
        last_wake,
        ignored: 0,
    }
}

#[test]
fn a_time_reads_as_local_date_time_and_offset() {
    let eastern = FixedOffset::west_opt(4 * 3600).unwrap();
    assert_eq!(
        readable_time_in(1_793_015_770, &eastern),
        "2026-10-26 07:56:10 -04:00"
    );
    assert_eq!(
        readable_time_in(1_793_015_770, &chrono::Utc),
        "2026-10-26 11:56:10 +00:00"
    );
    assert!(
        chrono::Utc
            .timestamp_opt(1_793_015_770, 0)
            .single()
            .is_some()
    );
}

#[test]
fn an_unrepresentable_time_prints_as_the_number() {
    assert_eq!(
        readable_time_in(u64::MAX, &chrono::Utc),
        u64::MAX.to_string()
    );
}

#[test]
fn status_prints_wake_times_and_never_bare_unix_seconds() {
    let wake = WakeRecord {
        started_at: 1_793_015_770,
        ended_at: Some(1_793_015_773),
        outcome: Some("success".into()),
        ..WakeRecord::default()
    };
    let lines = status_lines(&view(Some(wake)), true);
    let last = lines.last().unwrap();
    assert!(last.starts_with("last wake started 2026-10-"), "{last}");
    assert!(last.ends_with("outcome success"), "{last}");
    assert!(!last.contains("1793015770"), "{last}");
}

#[test]
fn an_unfinished_wake_says_in_flight() {
    let wake = WakeRecord {
        started_at: 1_793_015_770,
        ..WakeRecord::default()
    };
    let lines = status_lines(&view(Some(wake)), true);
    assert!(
        lines
            .last()
            .unwrap()
            .contains("ended in-flight outcome none")
    );
    assert_eq!(
        status_lines(&view(None), false).last().unwrap(),
        "last wake none"
    );
}

fn parked_detail() -> DecisionDetail {
    DecisionDetail {
        kind: "park".into(),
        id: "park n1 r1 parked t1 a0".into(),
        need: "n1".into(),
        run: 1,
        node: "parked".into(),
        task: "t1".into(),
        issue: Some("cto-unanswered".into()),
        reason: "the run parked at `parked`".into(),
        evidence: vec![
            "planner step t0 verdict park".into(),
            "raised cto-unanswered on t1".into(),
        ],
        controls: vec![
            "reply on the need thread to restart the run at plan".into(),
            "vivi pump answer t1".into(),
        ],
    }
}

#[test]
fn a_decision_report_shows_identity_issue_references_and_choices() {
    let lines = decision_lines(&parked_detail());
    assert_eq!(
        lines[0],
        "  decision park n1 r1 parked t1 a0 kind park need n1 r1 task t1 issue cto-unanswered"
    );
    assert_eq!(lines[1], "    evidence planner step t0 verdict park");
    assert_eq!(lines[2], "    evidence raised cto-unanswered on t1");
    assert_eq!(
        lines[3],
        "    choice reply on the need thread to restart the run at plan"
    );
    assert_eq!(lines[4], "    choice vivi pump answer t1");
}

#[test]
fn a_decision_without_an_issue_states_it() {
    let detail = DecisionDetail {
        issue: None,
        ..parked_detail()
    };
    let lines = decision_lines(&detail);
    assert!(lines[0].ends_with("issue none"), "{}", lines[0]);
}

#[test]
fn decision_details_serialize_additively_and_legacy_rows_load() {
    let value = serde_json::to_value(parked_detail()).unwrap();
    assert_eq!(value["run"], 1);
    assert_eq!(value["issue"], "cto-unanswered");
    assert_eq!(
        value["controls"],
        serde_json::json!([
            "reply on the need thread to restart the run at plan",
            "vivi pump answer t1"
        ])
    );
    // A detail persisted before these fields keeps loading, defaulted.
    let legacy = r#"{"kind":"lock","id":"lock n1 r1 locked t a0","need":"n1",
        "node":"locked","task":"t","reason":"why","evidence":[]}"#;
    let detail: DecisionDetail = serde_json::from_str(legacy).unwrap();
    assert_eq!(detail.run, 0);
    assert_eq!(detail.issue, None);
    assert!(detail.controls.is_empty());
}
