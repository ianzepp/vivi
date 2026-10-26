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
        started_at: Some(0),
        running_secs: Some(90),
        budget_secs: Some(1800),
        finished_at: None,
        stale_age_secs: None,
        read_only: Some(false),
        remote: false,
        tail: None,
    }
}

/// A pump control header: kind `Pump`, no task, empty slot.
fn pump_status_row(state: RowState) -> MonitorRow {
    let mut r = row(state);
    r.kind = RowKind::Pump;
    r.pool = "pump".to_string();
    r.slot = String::new();
    r.task = None;
    r.role = "running".to_string();
    r
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
    r.started_at = None;
    assert_eq!(activity_key(&r), "proj|Slot|here|slot-1|None|None");
}

#[test]
fn activity_key_distinguishes_row_kind() {
    let mut pump = row(RowState::Running);
    pump.kind = RowKind::Pump;
    assert_ne!(activity_key(&row(RowState::Running)), activity_key(&pump));
    assert!(activity_key(&pump).starts_with("proj|Pump|"));
}

#[test]
fn activity_key_distinguishes_retry_start_times() {
    let mut retry = row(RowState::Running);
    retry.started_at = Some(500);
    assert_ne!(activity_key(&row(RowState::Running)), activity_key(&retry));
    assert!(activity_key(&retry).ends_with("|Some(500)"));
}

#[test]
fn recent_finished_window_is_300_seconds() {
    assert_eq!(RECENT_FINISHED_SECS, 300);
}

#[test]
fn recent_activity_always_keeps_running_rows() {
    let mut old = row(RowState::Running);
    old.started_at = Some(0);
    assert!(recent_activity(&old, 10_000));
}

#[test]
fn recent_activity_cutoff_is_inclusive() {
    let mut r = row(RowState::Finished("pushed".to_string()));
    r.finished_at = Some(1_000);
    assert!(recent_activity(&r, 1_000 + RECENT_FINISHED_SECS));
    assert!(!recent_activity(&r, 1_000 + RECENT_FINISHED_SECS + 1));
}

#[test]
fn recent_activity_keeps_unknown_completion_finishes() {
    let mut r = row(RowState::Finished("uncollected".to_string()));
    r.finished_at = None;
    assert!(recent_activity(&r, 10_000));
}

#[test]
fn pump_status_header_is_distinguished_from_ticks_and_slots() {
    assert!(is_pump_status(&pump_status_row(RowState::Running)));
    assert!(is_pump_status(&pump_status_row(RowState::Finished(
        "stopped".to_string()
    ))));
    let mut tick = pump_status_row(RowState::Running);
    tick.slot = "tick".to_string();
    tick.task = Some("t1".to_string());
    assert!(!is_pump_status(&tick));
    assert!(!is_pump_status(&row(RowState::Running)));
}

#[test]
fn pump_status_header_exempt_from_expiry_when_stopped() {
    let mut stopped = pump_status_row(RowState::Finished("stopped".to_string()));
    stopped.finished_at = Some(0);
    assert!(recent_activity(&stopped, RECENT_FINISHED_SECS + 1));
}

#[test]
fn running_text_appends_stale_label_when_stale() {
    let mut r = row(RowState::Finished("pushed".to_string()));
    r.stale_age_secs = Some(42);
    assert_eq!(running_text(&r), "1m30s/30m (stale 42s)");
}

#[test]
fn running_text_labels_zero_stale_age() {
    let mut r = row(RowState::Running);
    r.stale_age_secs = Some(0);
    assert_eq!(running_text(&r), "1m30s/30m (stale 0s)");
}

#[test]
fn running_text_stale_age_grows_with_age() {
    let mut r = row(RowState::Finished("failed".to_string()));
    r.stale_age_secs = Some(90);
    assert_eq!(running_text(&r), "1m30s/30m (stale 1m30s)");
    r.stale_age_secs = Some(150);
    assert_eq!(running_text(&r), "1m30s/30m (stale 2m30s)");
}

#[test]
fn additive_completion_fields_serialize_as_null_when_absent() {
    let text = serde_json::to_string(&row(RowState::Running)).unwrap();
    assert!(text.contains("\"finished_at\":null"), "{text}");
    assert!(text.contains("\"stale_age_secs\":null"), "{text}");
}

#[test]
fn additive_completion_fields_serialize_when_present() {
    let mut r = row(RowState::Finished("pushed".to_string()));
    r.finished_at = Some(1060);
    r.stale_age_secs = Some(42);
    let text = serde_json::to_string(&r).unwrap();
    assert!(text.contains("\"finished_at\":1060"), "{text}");
    assert!(text.contains("\"stale_age_secs\":42"), "{text}");
}

#[test]
fn row_state_shape_is_unchanged_by_additive_fields() {
    let running = serde_json::to_string(&row(RowState::Running)).unwrap();
    assert!(running.contains("\"state\":\"running\""), "{running}");
    let finished = serde_json::to_string(&row(RowState::Finished("pushed".to_string()))).unwrap();
    assert!(
        finished.contains("\"state\":{\"finished\":\"pushed\"}"),
        "{finished}"
    );
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
