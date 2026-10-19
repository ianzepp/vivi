use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{Action, ListView, cell_text};
use crate::monitor::model::{MonitorRow, RowKind, RowState};

#[test]
fn list_keys_select_rows_open_tail_and_quit() {
    let mut view = ListView::default();
    assert_eq!(
        view.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), 12),
        Action::Down
    );
    assert_eq!(view.selected, 1);
    assert_eq!(
        view.key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE), 12),
        Action::NextPage
    );
    assert_eq!(view.selected, 11);
    assert_eq!(
        view.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 12),
        Action::EnterTail
    );
    assert_eq!(
        view.key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE), 12),
        Action::Quit
    );
}

#[test]
fn row_cells_show_need_and_work_kind_in_distinct_columns() {
    let row = MonitorRow {
        project: "alpha".into(),
        kind: RowKind::Slot,
        pool: "local".into(),
        slot: "slot-1".into(),
        provider: "local".into(),
        role: "hand-one".into(),
        task: Some("abcd1234".into()),
        need: Some("need1234".into()),
        work_kind: Some("fix".into()),
        subject: Some("Fix the thing".into()),
        model: "model-x".into(),
        state: RowState::Running,
        started_at: Some(100),
        running_secs: Some(10),
        budget_secs: Some(600),
        read_only: Some(false),
        remote: false,
        tail: None,
    };
    let cells = cell_text(&row);
    assert_eq!(cells[2], "fix");
    assert_eq!(cells[3], "need1234");
}
