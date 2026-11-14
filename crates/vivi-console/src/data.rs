//! What the console reads: the mailspace store and the project config.
//!
//! Everything here is blocking. Pages call it from `spawn_blocking`.

pub mod config;
pub mod failures;
pub mod job;
pub mod job_notes;
pub mod jobs;
pub mod json_list;
pub mod letter;
pub mod mail;
pub mod mail_body;
pub mod mail_read;
pub mod need_flow;
pub mod need_page;
pub mod needs;
pub mod overview;
pub mod pools;
pub mod pump_db;
pub mod pump_page;
pub mod runs;
pub mod slots;
pub mod store;

use std::path::Path;

use rusqlite::Connection;
use vivi_error::VivariumError;

/// The slot index table the console pages read.
pub const SLOT_RUNS_TABLE: &str = "slot_runs";

/// What reading the slot index found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunsState<T> {
    /// The project has no mailspace store yet.
    NoStore,
    /// The store predates the slot index (no `slot_runs` table).
    NoSlotRuns,
    Ready(T),
}

impl<T> RunsState<T> {
    /// Map the loaded value, keeping the two empty states.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> RunsState<U> {
        match self {
            Self::NoStore => RunsState::NoStore,
            Self::NoSlotRuns => RunsState::NoSlotRuns,
            Self::Ready(value) => RunsState::Ready(f(value)),
        }
    }
}

/// Open the project's store read-only and run `f` inside one read
/// transaction, unless the store or its `slot_runs` table is missing.
///
/// # Errors
/// Returns an error when the store exists but cannot be opened or read.
pub fn with_runs<T>(
    root: &Path,
    f: impl FnOnce(&Connection) -> Result<T, VivariumError>,
) -> Result<RunsState<T>, VivariumError> {
    let Some(store) = store::ReadStore::open(root)? else {
        return Ok(RunsState::NoStore);
    };
    store.read(|conn| {
        if !store::has_table(conn, SLOT_RUNS_TABLE)? {
            return Ok(RunsState::NoSlotRuns);
        }
        f(conn).map(RunsState::Ready)
    })
}
