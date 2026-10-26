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
fn activity_key_distinguishes_kinds_and_retry_starts() {
    let base = row(RowState::Running);
    let mut pump = base.clone();
    pump.kind = RowKind::Pump;
    assert_ne!(activity_key(&base), activity_key(&pump));
    let mut retry = base.clone();
    retry.started_at = Some(500);
    assert_ne!(activity_key(&base), activity_key(&retry));
    assert_eq!(
        activity_key(&retry),
        "proj|Slot|here|slot-1|Some(\"a1b2c3d4\")|Some(500)"
    );
}

#[test]
fn recent_finished_window_is_shared_300s() {
    assert_eq!(RECENT_FINISHED_SECS, 300);
}

#[test]
fn recent_activity_passes_running_rows_at_any_age() {
    let mut r = row(RowState::Running);
    r.started_at = Some(0);
    r.finished_at = None;
    assert!(recent_activity(&r, 100_000));
}

#[test]
fn recent_activity_cutoff_is_inclusive() {
    let mut r = row(RowState::Finished("pushed".to_string()));
    r.finished_at = Some(1000);
    assert!(recent_activity(&r, 1000 + RECENT_FINISHED_SECS));
    assert!(!recent_activity(&r, 1000 + RECENT_FINISHED_SECS + 1));
}

#[test]
fn recent_activity_passes_unknown_completion_statelessly() {
    let mut r = row(RowState::Finished("uncollected".to_string()));
    r.finished_at = None;
    assert!(recent_activity(&r, 1_000_000));
}

#[test]
fn is_pump_status_matches_only_the_control_header() {
    let mut status = row(RowState::Finished("stopped".to_string()));
    status.kind = RowKind::Pump;
    status.pool = "pump".to_string();
    status.slot = String::new();
    status.task = None;
    assert!(is_pump_status(&status));
    let mut tick = status.clone();
    tick.slot = "tick".to_string();
    tick.task = Some("cycle-1".to_string());
    assert!(!is_pump_status(&tick));
    let mut taskless_tick = tick.clone();
    taskless_tick.task = None;
    assert!(!is_pump_status(&taskless_tick));
    let mut slotted_status = status.clone();
    slotted_status.slot = "tick".to_string();
    assert!(!is_pump_status(&slotted_status));
    assert!(!is_pump_status(&row(RowState::Running)));
}

#[test]
fn recent_activity_exempts_pump_status_headers() {
    let mut status = row(RowState::Finished("paused".to_string()));
    status.kind = RowKind::Pump;
    status.slot = String::new();
    status.task = None;
    status.finished_at = Some(0);
    assert!(is_pump_status(&status));
    assert!(recent_activity(&status, 1_000_000));
}

#[test]
fn running_text_appends_stale_label_at_zero_age() {
    let mut r = row(RowState::Finished("pushed".to_string()));
    r.running_secs = Some(90);
    r.stale_age_secs = Some(0);
    assert_eq!(running_text(&r), "1m30s/30m (stale 0s)");
}

#[test]
fn running_text_stale_label_grows_with_age() {
    let mut r = row(RowState::Finished("pushed".to_string()));
    r.running_secs = None;
    r.stale_age_secs = Some(750);
    assert_eq!(running_text(&r), "-/30m (stale 12m30s)");
    r.stale_age_secs = Some(4980);
    assert_eq!(running_text(&r), "-/30m (stale 1h23m)");
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
