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
    /// whose source never supplied `finished_at`. The stamp outlives the
    /// visible row — for as long as the activity stays observed — so repeated
    /// polls cannot restart the window after expiry. A different `started_at`
    /// is a new attempt with its own stamp. Never backfilled from
    /// `started_at`, which says when work began, not when it ended, and never
    /// written back into the authoritative `finished_at` field.
    first_finished: HashMap<String, u64>,
}

impl SessionHistory {
    /// Merge one poll into the session view. Currently observed rows carry
    /// themselves, except a finished row whose completion window has already
    /// expired: it stays invisible however often the poll re-lists it,
    /// because the completion time — `finished_at` when the source supplied
    /// one, otherwise the first observation of the finish — outlives the
    /// visible row. Finished rows linger for [`COMPLETION_WINDOW_SECS`] past
    /// completion. Running rows missing from the poll drop without a
    /// manufactured completion, and a running report for an activity already
    /// seen finished preserves the terminal state instead of reviving it.
    /// Pump status headers are mode reports, not completions, and never enter
    /// the window.
    pub fn refresh(&mut self, fresh: Vec<MonitorRow>, now: u64) {
        let current: HashSet<String> = fresh.iter().map(activity_key).collect();
        let mut prior: Vec<(String, MonitorRow)> = self
            .rows
            .drain(..)
            .map(|row| (activity_key(&row), row))
            .collect();
        let mut next = Vec::with_capacity(fresh.len());
        for row in fresh {
            let key = activity_key(&row);
            let previous = prior
                .iter()
                .position(|(old, _)| *old == key)
                .map(|at| prior.remove(at).1);
            if is_status_row(&row) {
                next.push(row);
                continue;
            }
            if is_finished(&row) {
                // Expired finishes stay gone however often re-observed.
                if !self.expired(&key, row.finished_at, now) {
                    next.push(row);
                }
                continue;
            }
            // Live row: an activity already seen finished stays finished.
            let done = previous.filter(|old| is_finished(old));
            if done.is_none() && !self.first_finished.contains_key(&key) {
                next.push(row);
                continue;
            }
            if let Some(done) = done {
                if !self.expired(&key, done.finished_at, now) {
                    next.push(done);
                }
            }
        }
        for (key, row) in prior {
            if is_status_row(&row) || !is_finished(&row) {
                continue;
            }
            if !self.expired(&key, row.finished_at, now) {
                next.push(row);
            }
        }
        self.stamp_undated_finishes(&next, &current, now);
        self.rows = next;
    }

    #[must_use]
    pub fn rows(&self) -> &[MonitorRow] {
        &self.rows
    }

    /// True when the activity finished strictly more than the window ago:
    /// the authoritative `finished_at` when supplied, else the inferred
    /// first-observed finish.
    fn expired(&self, key: &str, finished_at: Option<u64>, now: u64) -> bool {
        finished_at
            .or_else(|| self.first_finished.get(key).copied())
            .is_some_and(|at| now.saturating_sub(at) > COMPLETION_WINDOW_SECS)
    }

    /// Stamp newly seen undated finishes, keep the earliest stamp for rows
    /// already tracked, and drop stamps for identities no longer needed:
    /// keys neither visible in the history nor observed in this poll.
    fn stamp_undated_finishes(&mut self, rows: &[MonitorRow], current: &HashSet<String>, now: u64) {
        for row in rows {
            if !is_status_row(row) && is_finished(row) && row.finished_at.is_none() {
                self.first_finished.entry(activity_key(row)).or_insert(now);
            }
        }
        self.first_finished.retain(|key, _| {
            current.contains(key) || rows.iter().any(|row| &activity_key(row) == key)
        });
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
