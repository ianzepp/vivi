//! Session history: observed rows plus a five-minute finished-activity window.

use std::collections::{HashMap, HashSet};

use super::model::{MonitorRow, RowKind, activity_key, is_finished};

/// Finished activities linger this many seconds past their completion time;
/// they expire once `now - completion` is strictly greater.
const COMPLETION_WINDOW_SECS: u64 = 300;

#[derive(Debug, Default)]
pub struct SessionHistory {
    rows: Vec<MonitorRow>,
    /// First epoch second each undated finished activity was observed
    /// finished, by activity key: the completion time of last resort for rows
    /// whose source never supplied `finished_at`. Never backfilled from
    /// `started_at`, which says when work began, not when it ended.
    first_finished: HashMap<String, u64>,
}

impl SessionHistory {
    /// Merge one poll into the session view. Currently observed rows always
    /// carry themselves; running rows missing from the poll drop without a
    /// manufactured completion; finished rows linger for
    /// [`COMPLETION_WINDOW_SECS`] past their completion time — `finished_at`
    /// when the source supplied one, otherwise the first observation of the
    /// finish. Pump status headers are mode reports, not completions, and
    /// never enter the window.
    pub fn refresh(&mut self, fresh: Vec<MonitorRow>, now: u64) {
        let current: HashSet<String> = fresh.iter().map(activity_key).collect();
        let mut next = fresh;
        for old in self.rows.drain(..) {
            let key = activity_key(&old);
            if current.contains(&key) || !is_finished(&old) {
                continue;
            }
            let completed = old
                .finished_at
                .or_else(|| self.first_finished.get(&key).copied());
            if completed.is_some_and(|at| now.saturating_sub(at) <= COMPLETION_WINDOW_SECS) {
                next.push(old);
            }
        }
        self.stamp_undated_finishes(&next, now);
        self.rows = next;
    }

    #[must_use]
    pub fn rows(&self) -> &[MonitorRow] {
        &self.rows
    }

    /// Stamp newly seen undated finishes, keep the earliest stamp for rows
    /// already tracked, and drop stamps whose activity left the history.
    fn stamp_undated_finishes(&mut self, rows: &[MonitorRow], now: u64) {
        for row in rows {
            if !is_status_row(row) && is_finished(row) && row.finished_at.is_none() {
                self.first_finished.entry(activity_key(row)).or_insert(now);
            }
        }
        self.first_finished
            .retain(|key, _| rows.iter().any(|row| &activity_key(row) == key));
    }
}

/// The pump service status header (no task of its own): it reports the
/// current mode and stays only while observed, whatever the mode says.
fn is_status_row(row: &MonitorRow) -> bool {
    row.kind == RowKind::Pump && row.task.is_none()
}

#[cfg(test)]
#[path = "history_test.rs"]
mod tests;
