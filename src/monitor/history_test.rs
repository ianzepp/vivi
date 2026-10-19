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
        running_secs: Some(1),
        budget_secs: Some(600),
        read_only: Some(false),
        remote: false,
        tail: None,
    }
}

#[test]
fn missing_running_rows_turn_gray_and_remain_in_session_history() {
    let mut history = SessionHistory::default();
    history.refresh(vec![row("a"), row("b")], |_| None);
    history.refresh(vec![row("a")], |old| {
        (old.task.as_deref() == Some("b")).then(|| "pushed branch".into())
    });
    assert_eq!(history.rows().len(), 2);
    assert!(
        history
            .rows()
            .iter()
            .any(|r| r.state == RowState::Finished("pushed branch".into()))
    );
    history.refresh(vec![row("a")], |_| None);
    assert_eq!(history.rows().len(), 2);
}
