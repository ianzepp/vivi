//! Read-only pump summaries for `need show` and `boot`.
//!
//! The pump owns `pump.db`; these commands only read it. A missing database
//! means "pump state is not local", and an unreadable one is reported as
//! such. Neither ever fails the calling command.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::VivariumError;

use super::state::State;
use super::store::{DB_FILENAME, PumpStore, SCHEMA_VERSION};

/// What a reader found at the pump home.
pub enum PumpRead {
    /// No `pump.db` on this host.
    Missing,
    /// The file exists but cannot be read, with the reason.
    Unreadable(String),
    /// A snapshot of the pump's state.
    Ready(Snapshot),
}

/// One read of `pump.db`.
pub struct Snapshot {
    state: State,
    exceptions: Vec<(u64, String, String, String)>,
}

/// One role's latest cadence run, for read-only board and status views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CadenceRunSummary {
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub outcome: Option<String>,
}

/// Read the pump database at `home`.
///
/// # Errors
/// Returns [`VivariumError`] only when the pump home path cannot be resolved;
/// a missing or unreadable database is a [`PumpRead`] variant.
pub fn read(home: &Path) -> Result<PumpRead, VivariumError> {
    let path = home.join(DB_FILENAME);
    let Some(store) = PumpStore::open_read_only(&path)? else {
        return Ok(PumpRead::Missing);
    };
    let version = match store.schema_version() {
        Ok(version) => version,
        Err(err) => return Ok(PumpRead::Unreadable(err.to_string())),
    };
    if version != SCHEMA_VERSION {
        return Ok(PumpRead::Unreadable(format!(
            "schema {version}, expected {SCHEMA_VERSION}"
        )));
    }
    match store.load_snapshot() {
        Ok((state, exceptions)) => Ok(PumpRead::Ready(Snapshot { state, exceptions })),
        Err(err) => Ok(PumpRead::Unreadable(err.to_string())),
    }
}

impl Snapshot {
    /// Latest cadence-run receipts keyed by role name.
    #[must_use]
    pub fn cadence_runs(&self) -> BTreeMap<String, CadenceRunSummary> {
        crate::pump::cadence::state(&self.state)
            .runs
            .into_iter()
            .map(|(role, run)| {
                (
                    role,
                    CadenceRunSummary {
                        started_at: run.started_at,
                        ended_at: run.ended_at,
                        outcome: run.outcome,
                    },
                )
            })
            .collect()
    }

    /// Exception rows, oldest first: `(at, kind, unit, text)`.
    #[must_use]
    pub fn exception_rows(&self) -> &[(u64, String, String, String)] {
        &self.exceptions
    }
}
