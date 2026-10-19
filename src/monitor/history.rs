//! Session-only retention of rows that disappear after a lane finishes.

use std::collections::HashSet;

use super::model::{MonitorRow, RowState, activity_key, is_finished};

#[derive(Debug, Default)]
pub struct SessionHistory {
    rows: Vec<MonitorRow>,
}

impl SessionHistory {
    pub fn refresh(
        &mut self,
        fresh: Vec<MonitorRow>,
        outcome: impl Fn(&MonitorRow) -> Option<String>,
    ) {
        let current: HashSet<String> = fresh.iter().map(activity_key).collect();
        let mut next = fresh;
        for mut old in self.rows.drain(..) {
            if current.contains(&activity_key(&old)) {
                continue;
            }
            if !is_finished(&old) {
                let Some(outcome) = outcome(&old) else {
                    next.push(old);
                    continue;
                };
                old.state = RowState::Finished(outcome);
            }
            next.push(old);
        }
        self.rows = next;
    }

    #[must_use]
    pub fn rows(&self) -> &[MonitorRow] {
        &self.rows
    }
}

#[cfg(test)]
#[path = "history_test.rs"]
mod tests;
