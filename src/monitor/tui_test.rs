use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;

use super::{Action, Freshness, ListView, cell_text, freshness, freshness_counts, render};
use crate::monitor::model::{
    MonitorRow, RowKind, RowState, format_secs, running_text, started_text,
};

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
        stale_age_secs: None,
        finished_at: None,
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

fn sample_row() -> MonitorRow {
    MonitorRow {
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
        stale_age_secs: None,
        finished_at: None,
        running_secs: Some(10),
        budget_secs: Some(600),
        read_only: Some(false),
        remote: false,
        tail: None,
    }
}

#[test]
fn stale_row_cells_show_age_and_last_known_state() {
    let stale_running = sample_row();
    let cells = cell_text(&MonitorRow {
        stale_age_secs: Some(20),
        ..stale_running.clone()
    });
    assert_eq!(cells[9], "stale 20s (running)");
    // Freshness is the only rendered change: identity, start, and frozen
    // run/budget values stay untouched.
    assert_eq!(cells[7], "abcd1234 Fix the thing");
    assert_eq!(cells[10], started_text(Some(100)));
    assert_eq!(cells[11], running_text(&stale_running));

    let stale_finished = MonitorRow {
        state: RowState::Finished("pushed".into()),
        stale_age_secs: Some(20),
        ..sample_row()
    };
    assert_eq!(cell_text(&stale_finished)[9], "stale 20s (pushed)");

    assert_eq!(cell_text(&sample_row())[9], "running");
}

#[test]
fn freshness_classes_partition_rows_and_stale_never_counts_live() {
    let stale_running = MonitorRow {
        stale_age_secs: Some(45),
        ..sample_row()
    };
    let stale_finished = MonitorRow {
        state: RowState::Finished("pushed".into()),
        stale_age_secs: Some(45),
        ..sample_row()
    };
    let finished = MonitorRow {
        state: RowState::Finished("failed".into()),
        ..sample_row()
    };
    assert_eq!(freshness(&sample_row()), Freshness::Live);
    assert_eq!(freshness(&stale_running), Freshness::Stale);
    assert_eq!(freshness(&stale_finished), Freshness::Finished);
    assert_eq!(freshness(&finished), Freshness::Finished);
    assert_eq!(
        freshness_counts(&[sample_row(), stale_running, stale_finished, finished]),
        (1, 1, 2)
    );
    assert_eq!(freshness_counts(&[]), (0, 0, 0));
}

fn rendered(rows: &[MonitorRow]) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(200, 8)).expect("test terminal");
    terminal
        .draw(|frame| render(frame, frame.area(), rows, 0, 1))
        .expect("test draw");
    terminal.backend().buffer().clone()
}

fn buffer_line(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width)
        .map(|x| buffer.get(x, y).symbol())
        .collect()
}

#[test]
fn title_reports_live_stale_and_finished_counts_separately() {
    let stale_running = MonitorRow {
        stale_age_secs: Some(45),
        ..sample_row()
    };
    let stale_finished = MonitorRow {
        state: RowState::Finished("pushed".into()),
        stale_age_secs: Some(45),
        ..sample_row()
    };
    let finished = MonitorRow {
        state: RowState::Finished("failed".into()),
        ..sample_row()
    };
    let buffer = rendered(&[sample_row(), stale_running, stale_finished, finished]);
    let title = buffer_line(&buffer, 0);
    assert!(
        title.contains("4 row(s) · 1 live · 1 stale · 2 finished"),
        "title: {title}"
    );
}

#[test]
fn stale_rows_render_with_warning_style_and_finished_rows_stay_dim() {
    let stale_running = MonitorRow {
        stale_age_secs: Some(45),
        ..sample_row()
    };
    let finished = MonitorRow {
        state: RowState::Finished("pushed".into()),
        ..sample_row()
    };
    // y=0 title border, y=1 header, data rows from y=2.
    let buffer = rendered(&[sample_row(), stale_running, finished]);
    assert_eq!(buffer.get(1, 2).style().fg, Some(Color::Reset));
    assert_eq!(buffer.get(1, 3).style().fg, Some(Color::Yellow));
    assert_eq!(buffer.get(1, 4).style().fg, Some(Color::DarkGray));
}

#[test]
fn stale_age_uses_the_shared_duration_format() {
    let stale = MonitorRow {
        stale_age_secs: Some(3_725),
        ..sample_row()
    };
    assert_eq!(
        cell_text(&stale)[9],
        format!("stale {} (running)", format_secs(3_725))
    );
}
