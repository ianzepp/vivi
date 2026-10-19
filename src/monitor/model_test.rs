//! Unit tests for [`crate::monitor::model`].

use super::*;

fn row(state: RowState) -> MonitorRow {
    MonitorRow {
        project: "proj".to_string(),
        kind: RowKind::Slot,
        pool: "here".to_string(),
        slot: "slot-1".to_string(),
        provider: "local".to_string(),
        role: "hand".to_string(),
        task: Some("a1b2c3d4".to_string()),
        subject: Some("Fix the thing".to_string()),
        model: "test/model".to_string(),
        state,
        started_at: Some(0),
        running_secs: Some(90),
        budget_secs: Some(1800),
        read_only: Some(false),
        remote: false,
        tail: None,
    }
}

#[test]
fn running_text_formats_budget_pair() {
    assert_eq!(running_text(&row(RowState::Running)), "1m30s/30m");
}

#[test]
fn running_text_dash_when_unknown() {
    let mut r = row(RowState::Running);
    r.running_secs = None;
    r.budget_secs = None;
    assert_eq!(running_text(&r), "-");
}

#[test]
fn running_text_budget_without_running() {
    let mut r = row(RowState::Running);
    r.running_secs = None;
    assert_eq!(running_text(&r), "-/30m");
}

#[test]
fn running_text_running_without_budget() {
    let mut r = row(RowState::Running);
    r.budget_secs = None;
    assert_eq!(running_text(&r), "1m30s");
}

#[test]
fn format_secs_tiers() {
    assert_eq!(format_secs(45), "45s");
    assert_eq!(format_secs(90), "1m30s");
    assert_eq!(format_secs(750), "12m30s");
    assert_eq!(format_secs(720), "12m");
    assert_eq!(format_secs(4980), "1h23m");
}

#[test]
fn started_text_none_is_dash() {
    assert_eq!(started_text(None), "-");
}

#[test]
fn started_text_zero_is_utc_midnight() {
    assert_eq!(started_text(Some(0)), "00:00");
}

#[test]
fn activity_key_is_stable_shape() {
    assert_eq!(
        activity_key(&row(RowState::Running)),
        "proj|here|slot-1|Some(\"a1b2c3d4\")"
    );
    let mut r = row(RowState::Running);
    r.task = None;
    assert_eq!(activity_key(&r), "proj|here|slot-1|None");
}

#[test]
fn row_state_serializes() {
    assert_eq!(serde_json::to_string(&RowState::Running).unwrap(), "\"running\"");
    assert_eq!(
        serde_json::to_string(&RowState::Finished("pushed".to_string())).unwrap(),
        "{\"finished\":\"pushed\"}"
    );
}

#[test]
fn is_finished_matches_outcome_states() {
    assert!(!is_finished(&row(RowState::Running)));
    assert!(is_finished(&row(RowState::Finished("failed".to_string()))));
}

#[test]
fn row_kind_serializes_lowercase() {
    assert_eq!(serde_json::to_string(&RowKind::Pump).unwrap(), "\"pump\"");
    assert_eq!(serde_json::to_string(&RowKind::Slot).unwrap(), "\"slot\"");
}
