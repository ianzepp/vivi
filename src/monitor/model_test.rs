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
        need: None,
        work_kind: None,
        subject: Some("Fix the thing".to_string()),
        model: "test/model".to_string(),
        state,
        finished_at: None,
        stale_age_secs: None,
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
        "proj|Slot|here|slot-1|Some(\"a1b2c3d4\")|Some(0)"
    );
    let mut r = row(RowState::Running);
    r.task = None;
    assert_eq!(activity_key(&r), "proj|Slot|here|slot-1|None|Some(0)");
}

#[test]
fn activity_key_separates_retries_in_the_same_slot() {
    let first = row(RowState::Running);
    let mut retry = row(RowState::Running);
    retry.started_at = Some(60);
    assert_ne!(activity_key(&first), activity_key(&retry));
}

#[test]
fn activity_key_separates_row_kinds() {
    let slot = row(RowState::Running);
    let mut pump = row(RowState::Running);
    pump.kind = RowKind::Pump;
    assert_ne!(activity_key(&slot), activity_key(&pump));
}

#[test]
fn activity_key_stable_across_freshness_and_lifecycle_changes() {
    let running = row(RowState::Running);
    let mut later = row(RowState::Finished("pushed".to_string()));
    later.finished_at = Some(400);
    later.stale_age_secs = Some(120);
    later.running_secs = Some(400);
    later.budget_secs = None;
    assert_eq!(activity_key(&running), activity_key(&later));
}

#[test]
fn finished_window_boundary_is_inclusive() {
    let mut r = row(RowState::Finished("pushed".to_string()));
    r.finished_at = Some(1000);
    assert!(recently_finished(&r, 1299));
    assert!(recently_finished(&r, 1300));
    assert!(!recently_finished(&r, 1301));
}

#[test]
fn missing_completion_cannot_establish_recency() {
    let mut r = row(RowState::Finished("pushed".to_string()));
    r.finished_at = None;
    r.stale_age_secs = Some(0);
    assert!(!recently_finished(&r, 1000));
}

#[test]
fn row_serializes_optional_metadata_when_present() {
    let mut r = row(RowState::Finished("pushed".to_string()));
    r.finished_at = Some(400);
    r.stale_age_secs = Some(120);
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(json["finished_at"], 400);
    assert_eq!(json["stale_age_secs"], 120);
}

#[test]
fn row_serializes_absent_optional_metadata_as_null() {
    let json = serde_json::to_value(&row(RowState::Running)).unwrap();
    assert!(json["finished_at"].is_null());
    assert!(json["stale_age_secs"].is_null());
}

#[test]
fn row_state_serializes() {
    assert_eq!(
        serde_json::to_string(&RowState::Running).unwrap(),
        "\"running\""
    );
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
