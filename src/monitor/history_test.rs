use super::super::model::{MonitorRow, RowKind, RowState};
use super::SessionHistory;

fn row(task: &str) -> MonitorRow {
    MonitorRow {
        project: "alpha".into(),
        kind: RowKind::Slot,
        pool: "local".into(),
        slot: "slot-1".into(),
        provider: "local".into(),
        role: "hand".into(),
        task: Some(task.into()),
        need: None,
        work_kind: None,
        subject: None,
        model: "m".into(),
        state: RowState::Running,
        started_at: Some(100),
        stale_age_secs: None,
        finished_at: None,
        running_secs: Some(1),
        budget_secs: Some(600),
        read_only: Some(false),
        remote: false,
        tail: None,
    }
}

fn finished(mut row: MonitorRow, outcome: &str, finished_at: Option<u64>) -> MonitorRow {
    row.state = RowState::Finished(outcome.into());
    row.finished_at = finished_at;
    row
}

fn pump_header(mode: &str) -> MonitorRow {
    MonitorRow {
        project: "alpha".into(),
        kind: RowKind::Pump,
        pool: "pump".into(),
        slot: String::new(),
        provider: "pump".into(),
        role: mode.into(),
        task: None,
        need: None,
        work_kind: None,
        subject: None,
        model: String::new(),
        state: RowState::Finished(mode.into()),
        started_at: None,
        stale_age_secs: None,
        finished_at: None,
        running_secs: None,
        budget_secs: None,
        read_only: None,
        remote: false,
        tail: None,
    }
}

#[test]
fn missing_running_rows_are_removed_without_manufacturing_a_completion() {
    let mut history = SessionHistory::default();
    history.refresh(vec![row("a"), row("b")], 100);
    history.refresh(vec![row("a")], 101);
    assert_eq!(history.rows().len(), 1);
    assert_eq!(history.rows()[0].state, RowState::Running);
    history.refresh(Vec::new(), 102);
    assert!(history.rows().is_empty());
}

#[test]
fn dated_finished_rows_linger_five_minutes_then_expire() {
    let done = finished(row("a"), "pushed", Some(1000));
    let mut history = SessionHistory::default();
    history.refresh(vec![done.clone()], 1100);
    history.refresh(Vec::new(), 1250);
    assert_eq!(history.rows(), &[done.clone()]);
    history.refresh(Vec::new(), 1600);
    assert!(history.rows().is_empty());
}

#[test]
fn expiry_boundary_is_exactly_five_minutes_after_completion() {
    let done = finished(row("a"), "pushed", Some(1000));
    let mut history = SessionHistory::default();
    history.refresh(vec![done.clone()], 1300);
    history.refresh(Vec::new(), 1300);
    assert_eq!(history.rows().len(), 1);
    history.refresh(vec![done], 1301);
    history.refresh(Vec::new(), 1301);
    assert!(history.rows().is_empty());
}

#[test]
fn old_start_with_recent_finish_expires_by_finish_not_start() {
    let mut done = finished(row("a"), "pushed", Some(1000));
    done.started_at = Some(0);
    let mut history = SessionHistory::default();
    history.refresh(vec![done.clone()], 1005);
    history.refresh(Vec::new(), 1005);
    assert_eq!(history.rows(), &[done.clone()]);
    history.refresh(vec![done], 1301);
    history.refresh(Vec::new(), 1301);
    assert!(history.rows().is_empty());
}

#[test]
fn undated_finished_rows_age_from_first_observed_finish_not_start() {
    let done = finished(row("a"), "pushed", None);
    let mut history = SessionHistory::default();
    history.refresh(vec![done.clone()], 500);
    history.refresh(vec![done.clone()], 600);
    history.refresh(Vec::new(), 800);
    assert_eq!(history.rows(), &[done.clone()]);
    history.refresh(Vec::new(), 801);
    assert!(history.rows().is_empty());
}

#[test]
fn current_pump_headers_never_expire_and_outlive_missing_tick_rows() {
    let tick = finished(
        MonitorRow {
            kind: RowKind::Pump,
            pool: "pump".into(),
            slot: "tick".into(),
            provider: "pump".into(),
            role: "cycle".into(),
            work_kind: Some("tick".into()),
            task: Some("tick-1".into()),
            started_at: Some(0),
            running_secs: Some(5),
            ..pump_header("idle")
        },
        "ok",
        None,
    );
    let mut history = SessionHistory::default();
    history.refresh(vec![pump_header("idle"), tick], 0);
    history.refresh(vec![pump_header("paused")], 10_000);
    let rows = history.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, RowState::Finished("paused".into()));
}

#[test]
fn pump_headers_survive_mode_changes_without_window_math() {
    let mut history = SessionHistory::default();
    history.refresh(vec![pump_header("running"), pump_header("stopped")], 0);
    history.refresh(vec![pump_header("running"), pump_header("stopped")], 50_000);
    assert_eq!(history.rows().len(), 2);
}

#[test]
fn repeated_observation_of_undated_finished_row_does_not_reset_age() {
    let done = finished(row("a"), "pushed", None);
    let mut history = SessionHistory::default();
    history.refresh(vec![done.clone()], 500);
    history.refresh(vec![done.clone()], 600);
    history.refresh(vec![done.clone()], 800);
    assert_eq!(history.rows(), &[done.clone()]);
    history.refresh(vec![done.clone()], 801);
    assert!(history.rows().is_empty());
    history.refresh(vec![done], 900);
    assert!(history.rows().is_empty());
}

#[test]
fn expired_undated_finish_stays_invisible_while_the_row_keeps_appearing() {
    let done = finished(row("a"), "pushed", None);
    let mut history = SessionHistory::default();
    history.refresh(vec![done.clone()], 0);
    history.refresh(vec![done.clone()], 100);
    history.refresh(vec![done.clone()], 301);
    assert!(history.rows().is_empty());
    history.refresh(vec![done.clone()], 400);
    history.refresh(vec![done.clone()], 60_000);
    assert!(history.rows().is_empty());
    history.refresh(Vec::new(), 60_001);
    assert!(history.rows().is_empty());
}

#[test]
fn a_new_attempt_receives_a_new_finish_age() {
    let first = finished(row("a"), "pushed", None);
    let mut retried = row("a");
    retried.started_at = Some(200);
    let second = finished(retried, "pushed", None);
    let mut history = SessionHistory::default();
    history.refresh(vec![first], 500);
    history.refresh(vec![second.clone()], 600);
    history.refresh(vec![second.clone()], 801);
    assert_eq!(history.rows(), &[second.clone()]);
    history.refresh(vec![second], 901);
    assert!(history.rows().is_empty());
}

#[test]
fn finished_activity_is_never_converted_back_to_running() {
    let done = finished(row("a"), "pushed", None);
    let live = row("a");
    let mut history = SessionHistory::default();
    history.refresh(vec![done.clone()], 500);
    history.refresh(vec![live.clone()], 700);
    assert_eq!(history.rows(), &[done.clone()]);
    assert_eq!(history.rows()[0].state, RowState::Finished("pushed".into()));
    history.refresh(vec![live.clone()], 801);
    assert!(history.rows().is_empty());
    history.refresh(vec![live], 60_000);
    assert!(history.rows().is_empty());
}
