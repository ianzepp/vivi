//! Two-stage terminal view: lane list, then a live log tail.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::{self, Event, KeyCode, KeyEvent};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Text;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};

use super::history::SessionHistory;
use super::model::{MonitorRow, RowState, running_text, started_text};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    PreviousPage,
    NextPage,
    EnterTail,
    Quit,
    None,
}

#[derive(Debug, Default)]
pub struct ListView {
    pub selected: usize,
}

impl ListView {
    pub fn key(&mut self, key: KeyEvent, len: usize) -> Action {
        let last = len.saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                Action::Up
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(last);
                Action::Down
            }
            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(10);
                Action::PreviousPage
            }
            KeyCode::PageDown => {
                self.selected = (self.selected + 10).min(last);
                Action::NextPage
            }
            KeyCode::Enter => Action::EnterTail,
            KeyCode::Esc | KeyCode::Char('q') => Action::Quit,
            _ => Action::None,
        }
    }
}

pub fn render(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    rows: &[MonitorRow],
    selected: usize,
    warning_count: usize,
) {
    let title = format!(
        " vivi monitor · {} row(s) · {} warning(s) · ↑/↓ select · enter tail · q quit ",
        rows.len(),
        warning_count
    );
    let block = Block::default().title(title).borders(Borders::ALL);
    let items = rows.iter().map(|item| {
        let mut row = Row::new(cells(item));
        if matches!(item.state, RowState::Finished(_)) {
            row = row.style(Style::default().fg(Color::DarkGray));
        }
        row
    });
    let table = Table::new(
        items,
        [
            Constraint::Length(12),
            Constraint::Length(8),
            Constraint::Length(15),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(24),
            Constraint::Length(20),
            Constraint::Min(12),
            Constraint::Length(8),
            Constraint::Length(13),
            Constraint::Length(4),
        ],
    )
    .header(
        Row::new([
            "project",
            "pool",
            "slot",
            "provider",
            "role",
            "task / subject",
            "model",
            "state",
            "started",
            "run / budget",
            "ro",
        ])
        .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .column_spacing(1)
    .block(block)
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default();
    state.select((!rows.is_empty()).then_some(selected.min(rows.len().saturating_sub(1))));
    frame.render_stateful_widget(table, area, &mut state);
}

fn cells(row: &MonitorRow) -> Vec<Cell<'static>> {
    let state = match &row.state {
        RowState::Running => "running".to_owned(),
        RowState::Finished(outcome) => outcome.clone(),
    };
    let provider = if row.remote {
        format!("{} remote", row.provider)
    } else {
        row.provider.clone()
    };
    let task = match (&row.task, &row.subject) {
        (Some(handle), Some(subject)) => format!("{handle} {subject}"),
        (Some(handle), None) => handle.clone(),
        (None, Some(subject)) => subject.clone(),
        (None, None) => String::new(),
    };
    [
        row.project.clone(),
        row.pool.clone(),
        row.slot.clone(),
        provider,
        row.role.clone(),
        task,
        row.model.clone(),
        state,
        started_text(row.started_at),
        running_text(row),
        row.read_only.map_or_else(
            || "-".into(),
            |read_only| if read_only { "ro" } else { "rw" }.into(),
        ),
    ]
    .into_iter()
    .map(Cell::from)
    .collect()
}

pub fn run(roots: &[PathBuf]) -> Result<(), crate::VivariumError> {
    let mut stdout = io::stdout();
    enable_raw_mode().map_err(io_error)?;
    if let Err(error) = execute!(stdout, EnterAlternateScreen) {
        restore_raw_terminal();
        return Err(io_error(error));
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => {
            restore_raw_terminal();
            return Err(io_error(error));
        }
    };
    let result = run_loop(&mut terminal, roots);
    restore_terminal(&mut terminal);
    result
}

fn restore_raw_terminal() {
    if let Err(error) = disable_raw_mode() {
        tracing::warn!(%error, "could not disable raw terminal mode");
    }
    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, LeaveAlternateScreen) {
        tracing::warn!(%error, "could not leave the alternate terminal screen");
    }
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) {
    if let Err(error) = disable_raw_mode() {
        tracing::warn!(%error, "could not disable raw terminal mode");
    }
    if let Err(error) = execute!(terminal.backend_mut(), LeaveAlternateScreen) {
        tracing::warn!(%error, "could not leave the alternate terminal screen");
    }
    if let Err(error) = terminal.show_cursor() {
        tracing::warn!(%error, "could not restore the terminal cursor");
    }
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    roots: &[PathBuf],
) -> Result<(), crate::VivariumError> {
    let mut history = SessionHistory::default();
    let mut view = ListView::default();
    let mut warnings = Vec::new();
    let mut last_poll = Instant::now() - Duration::from_secs(2);
    loop {
        if last_poll.elapsed() >= Duration::from_secs(1) {
            let snapshot = super::snapshot(roots, epoch_secs());
            history.refresh(snapshot.rows, |_| None);
            warnings = snapshot.warnings;
            last_poll = Instant::now();
        }
        terminal
            .draw(|frame| {
                let area = frame.area();
                render(frame, area, history.rows(), view.selected, warnings.len())
            })
            .map_err(io_error)?;
        if event::poll(Duration::from_millis(250)).map_err(io_error)? {
            let Event::Key(key) = event::read().map_err(io_error)? else {
                continue;
            };
            match view.key(key, history.rows().len()) {
                Action::Quit => return Ok(()),
                Action::EnterTail => {
                    if let Some(path) = history
                        .rows()
                        .get(view.selected)
                        .and_then(|row| row.tail.as_deref())
                    {
                        open_tail(terminal, path)?;
                    }
                }
                Action::Up
                | Action::Down
                | Action::PreviousPage
                | Action::NextPage
                | Action::None => {}
            }
        }
    }
}

fn open_tail(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    path: &Path,
) -> Result<(), crate::VivariumError> {
    let mut scroll = 0usize;
    loop {
        let text = read_tail(path);
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_tail(frame, area, path, &text, scroll)
            })
            .map_err(io_error)?;
        if event::poll(Duration::from_millis(500)).map_err(io_error)? {
            let Event::Key(key) = event::read().map_err(io_error)? else {
                continue;
            };
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
                KeyCode::PageUp => scroll = scroll.saturating_add(10),
                KeyCode::PageDown => scroll = scroll.saturating_sub(10),
                KeyCode::Home | KeyCode::Char('g') => scroll = 0,
                _ => {}
            }
        }
    }
}

fn read_tail(path: &Path) -> String {
    const MAX_BYTES: u64 = 256 * 1024;
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::from("Waiting for log output…");
    };
    let Ok(length) = file.metadata().map(|metadata| metadata.len()) else {
        return String::new();
    };
    let start = length.saturating_sub(MAX_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::with_capacity(length.saturating_sub(start) as usize);
    if file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        if let Some(newline) = text.find('\n') {
            text.drain(..=newline);
        }
    }
    text
}

fn render_tail(frame: &mut ratatui::Frame<'_>, area: Rect, path: &Path, text: &str, scroll: usize) {
    let height = area.height.saturating_sub(2) as usize;
    let lines: Vec<&str> = text.lines().collect();
    let end = lines.len().saturating_sub(scroll);
    let start = end.saturating_sub(height);
    let body = lines[start..end].join("\n");
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("log");
    let footer = if scroll == 0 {
        "follow · Esc back · PgUp/PgDn scroll"
    } else {
        "scrollback · Home follow · Esc back"
    };
    let block = Block::default()
        .title(format!(" {name} · {footer} "))
        .borders(Borders::ALL);
    frame.render_widget(
        Paragraph::new(Text::from(body))
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn io_error(error: io::Error) -> crate::VivariumError {
    crate::VivariumError::Other(format!("monitor terminal: {error}"))
}

#[cfg(test)]
#[path = "tui_test.rs"]
mod tests;
