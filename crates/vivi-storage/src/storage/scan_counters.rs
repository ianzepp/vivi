//! Row counters for the bulk loaders, for the scaling tests.
//!
//! A test cannot time a quadratic read reliably, but it can count the rows
//! the bulk loaders return: a fixed operation on a mailspace twice as large
//! should return about twice the rows, not four times. The counters exist
//! only with the `test-support` feature (or in this crate's own tests); in a
//! release build [`ScanCounters`] is a zero-size type and every method is a
//! no-op.

/// A snapshot of the counters: rows returned by each bulk loader.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanSnapshot {
    /// Rows from `work_graph_nodes` and `work_graph_nodes_by_subgraph`, plus
    /// one per node found by a point lookup.
    pub node_rows: u64,
    /// Rows from `work_graph_edges`.
    pub edge_rows: u64,
    /// Rows from the `list_messages*` loaders.
    pub message_rows: u64,
    /// Blobs read from disk.
    pub blobs_read: u64,
}

#[cfg(any(test, feature = "test-support"))]
impl ScanSnapshot {
    /// Every counted row and blob.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.node_rows + self.edge_rows + self.message_rows + self.blobs_read
    }

    /// The counts accumulated since `earlier`.
    #[must_use]
    pub fn since(&self, earlier: &Self) -> Self {
        Self {
            node_rows: self.node_rows - earlier.node_rows,
            edge_rows: self.edge_rows - earlier.edge_rows,
            message_rows: self.message_rows - earlier.message_rows,
            blobs_read: self.blobs_read - earlier.blobs_read,
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub(super) struct ScanCounters {
    cells: std::cell::Cell<ScanSnapshot>,
}

#[cfg(any(test, feature = "test-support"))]
impl ScanCounters {
    pub(super) fn snapshot(&self) -> ScanSnapshot {
        self.cells.get()
    }

    fn bump(&self, apply: impl FnOnce(&mut ScanSnapshot)) {
        let mut cells = self.cells.get();
        apply(&mut cells);
        self.cells.set(cells);
    }

    pub(super) fn nodes(&self, rows: usize) {
        self.bump(|c| c.node_rows += rows as u64);
    }

    pub(super) fn edges(&self, rows: usize) {
        self.bump(|c| c.edge_rows += rows as u64);
    }

    pub(super) fn messages(&self, rows: usize) {
        self.bump(|c| c.message_rows += rows as u64);
    }

    pub(super) fn blob(&self) {
        self.bump(|c| c.blobs_read += 1);
    }
}

#[cfg(not(any(test, feature = "test-support")))]
#[derive(Debug, Default)]
pub(super) struct ScanCounters;

#[cfg(not(any(test, feature = "test-support")))]
impl ScanCounters {
    #[inline]
    pub(super) fn nodes(&self, _rows: usize) {}
    #[inline]
    pub(super) fn edges(&self, _rows: usize) {}
    #[inline]
    pub(super) fn messages(&self, _rows: usize) {}
    #[inline]
    pub(super) fn blob(&self) {}
}
