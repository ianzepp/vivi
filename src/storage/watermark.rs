use super::{Storage, VivariumError};

/// A cheap fingerprint of "has the mailspace store changed since I last
/// looked". Two watermarks compare equal exactly when none of the covered
/// high-water marks moved, so a reader polls it and re-reads only on change.
///
/// It covers append-only growth:
/// - `messages`: the highest `messages` rowid, so new message rows (sends,
///   deliveries, replies, ingests);
/// - `events`: the highest `mailspace_events` id, so item events (moves such
///   as done and reopen, marks and unmarks, claims, notes, and every other
///   lifecycle step that logs an event);
/// - `graph_events`: the highest `work_graph_events` id, so work-graph
///   changes (import, apply, activate, complete, node state moves).
///
/// It does not cover in-place edits that log no event (for example a bare
/// read-state flip or a metadata overwrite). The ordering of values is
/// meaningless; compare for equality only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChangeWatermark {
    /// Highest `messages` rowid, or 0 when the table is empty.
    pub messages: i64,
    /// Highest `mailspace_events.event_id`, or 0 when the table is empty.
    pub events: i64,
    /// Highest `work_graph_events.event_id`, or 0 when the table is empty.
    pub graph_events: i64,
}

impl std::fmt::Display for ChangeWatermark {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.messages, self.events, self.graph_events)
    }
}

impl Storage {
    /// Read the store's change watermark: three `MAX` lookups on integer
    /// primary keys, each a single B-tree seek with no table scan.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if a query fails.
    pub fn change_watermark(&self) -> Result<ChangeWatermark, VivariumError> {
        let conn = self.conn.borrow();
        conn.query_row(
            "SELECT
               COALESCE((SELECT MAX(rowid) FROM messages), 0),
               COALESCE((SELECT MAX(event_id) FROM mailspace_events), 0),
               COALESCE((SELECT MAX(event_id) FROM work_graph_events), 0)",
            [],
            |row| {
                Ok(ChangeWatermark {
                    messages: row.get(0)?,
                    events: row.get(1)?,
                    graph_events: row.get(2)?,
                })
            },
        )
        .map_err(|e| VivariumError::Other(format!("failed to read change watermark: {e}")))
    }
}

#[cfg(test)]
#[path = "watermark_test.rs"]
mod tests;
