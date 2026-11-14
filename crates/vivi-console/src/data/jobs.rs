//! The jobs list's data: runs newest first, filtered and paged.

use std::path::Path;

use rusqlite::Connection;
use vivi_error::VivariumError;

use super::runs::{self, RunRow};
use super::{RunsState, with_runs};
use crate::queries::JOBS_LIST;

/// Rows on one page.
pub const PAGE_ROWS: i64 = 100;

/// How far back the list reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Hour,
    Day,
    Week,
}

impl Window {
    /// `1h`, `24h` or `7d`; anything else is `None`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "1h" => Some(Self::Hour),
            "24h" => Some(Self::Day),
            "7d" => Some(Self::Week),
            _ => None,
        }
    }

    /// The word the query string carries.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Hour => "1h",
            Self::Day => "24h",
            Self::Week => "7d",
        }
    }

    /// The window's length in seconds.
    #[must_use]
    pub fn secs(self) -> i64 {
        match self {
            Self::Hour => 3600,
            Self::Day => 24 * 3600,
            Self::Week => 7 * 24 * 3600,
        }
    }
}

/// The outcome filter words.
pub const OUTCOMES: [&str; 4] = ["finished", "timeout", "lost", "open"];

/// What the list is narrowed to. Empty text is no filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobFilter {
    pub outcome: Option<String>,
    pub failed: bool,
    pub pool: Option<String>,
    pub slot: Option<String>,
    pub model: Option<String>,
    /// A need handle prefix.
    pub need: Option<String>,
    pub window: Window,
}

impl Default for JobFilter {
    fn default() -> Self {
        Self {
            outcome: None,
            failed: false,
            pool: None,
            slot: None,
            model: None,
            need: None,
            window: Window::Day,
        }
    }
}

/// Where a page starts: below this dispatch time, and below this row id among
/// runs dispatched in the same second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub dispatched_at: i64,
    pub id: i64,
}

/// One listed run with its row id, which the next page's cursor needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobListRow {
    pub run: RunRow,
    pub id: i64,
}

/// One page of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobsPage {
    pub rows: Vec<JobListRow>,
    /// Where the next (older) page starts; `None` on the last page.
    pub next: Option<Cursor>,
}

/// Read one page of the project at `root`, as of `now` (epoch seconds).
///
/// # Errors
/// Returns an error when the store cannot be read.
pub fn load(
    root: &Path,
    now: i64,
    filter: &JobFilter,
    before: Option<Cursor>,
) -> Result<RunsState<JobsPage>, VivariumError> {
    with_runs(root, |conn| page(conn, now, filter, before))
}

fn page(
    conn: &Connection,
    now: i64,
    filter: &JobFilter,
    before: Option<Cursor>,
) -> Result<JobsPage, VivariumError> {
    let since = now - filter.window.secs();
    let (before_at, before_id) = before.map_or((i64::MAX, i64::MAX), |c| (c.dispatched_at, c.id));
    let outcome = filter.outcome.as_deref();
    let failed = i64::from(filter.failed);
    let limit = PAGE_ROWS + 1;
    let mut rows = runs::collect(
        conn,
        &JOBS_LIST,
        &[
            &since,
            &before_at,
            &before_id,
            &limit,
            &filter.pool,
            &filter.slot,
            &filter.model,
            &filter.need,
            &outcome,
            &failed,
        ],
        |row| {
            Ok(JobListRow {
                run: runs::run_from_row(row)?,
                id: row.get(16)?,
            })
        },
    )?;
    let more = rows.len() as i64 > PAGE_ROWS;
    rows.truncate(PAGE_ROWS as usize);
    let next = more.then(|| rows.last()).flatten().map(|last| Cursor {
        dispatched_at: last.run.dispatched_at,
        id: last.id,
    });
    Ok(JobsPage { rows, next })
}

#[cfg(test)]
#[path = "jobs_test.rs"]
mod tests;
