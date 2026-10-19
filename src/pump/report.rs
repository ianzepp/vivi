//! Read-only pump summaries for `need show` and `boot`.
//!
//! The pump owns `pump.db`; these commands only read it. A missing database
//! means "pump state is not local", and an unreadable one is reported as
//! such. Neither ever fails the calling command.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::error::VivariumError;

use super::state::{State, Status, Unit};
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
    exceptions: usize,
}

/// One need's pump state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedSummary {
    pub verdict: String,
    pub revision: u64,
    pub units_total: usize,
    pub counts: BTreeMap<String, usize>,
    pub complete: bool,
    pub amendment_pending: bool,
    pub last_event: Option<String>,
}

/// The whole pump's state, in counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSummary {
    pub units: BTreeMap<String, usize>,
    pub complete: usize,
    pub amendments: usize,
    pub stuck: usize,
    pub exceptions: usize,
    pub inflight: usize,
    /// Planners the board currently has in flight.
    pub planning: usize,
    /// Handles parked after exhausting planner retries.
    pub planner_parked: usize,
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
    match (store.load(), store.exceptions()) {
        (Ok(state), Ok(exceptions)) => Ok(PumpRead::Ready(Snapshot {
            state,
            exceptions: exceptions.len(),
        })),
        (Err(err), _) | (_, Err(err)) => Ok(PumpRead::Unreadable(err.to_string())),
    }
}

impl Snapshot {
    /// The loaded state, for the closeout and amendment passes.
    #[must_use]
    pub(crate) fn state(&self) -> &State {
        &self.state
    }

    /// The pump state for one need, when the pump has planned it.
    #[must_use]
    pub fn need(&self, handle: &str) -> Option<NeedSummary> {
        let record = self.state.needs.get(handle)?;
        let mut counts = BTreeMap::new();
        for unit in self.state.units.values().filter(|unit| unit.need == handle) {
            *counts.entry(unit.status.as_str().to_owned()).or_default() += 1;
        }
        let complete = need_complete(&self.state, handle);
        let last_event = self
            .state
            .units
            .iter()
            .filter(|(_, unit)| unit.need == handle)
            .max_by_key(|(_, unit)| unit.since)
            .map(|(id, unit)| format!("{id} {}", unit.status.as_str()));
        Some(NeedSummary {
            verdict: record.verdict.clone(),
            revision: record.revision,
            units_total: record.units.len(),
            counts,
            complete,
            amendment_pending: record.amendment_pending,
            last_event,
        })
    }

    /// Counts for the boot digest.
    #[must_use]
    pub fn summary(&self) -> StateSummary {
        let units = self.state.counts();
        let complete = self
            .state
            .needs
            .keys()
            .filter(|handle| need_complete(&self.state, handle))
            .count();
        StateSummary {
            stuck: units.get("stuck").copied().unwrap_or(0),
            inflight: units
                .get("inflight")
                .copied()
                .unwrap_or(0)
                .saturating_add(units.get("merging").copied().unwrap_or(0)),
            amendments: self
                .state
                .needs
                .values()
                .filter(|record| record.amendment_pending)
                .count(),
            planning: self.state.board.planning.len(),
            planner_parked: self.state.board.planner_failed.len(),
            units,
            complete,
            exceptions: self.exceptions,
        }
    }

    /// Need handles whose every unit is satisfied.
    #[must_use]
    pub fn complete_needs(&self) -> BTreeSet<String> {
        self.state
            .needs
            .keys()
            .filter(|handle| need_complete(&self.state, handle))
            .cloned()
            .collect()
    }
}

/// One `need show` block. `registry_open` is the item's Vivi folder state;
/// `None` means the caller could not determine it.
#[must_use]
pub fn render_need(summary: &NeedSummary, registry_open: Option<bool>) -> String {
    let rendered = render_counts(&summary.counts);
    let total = summary.units_total;
    let complete = if summary.complete { "yes" } else { "no" };
    let registry = match registry_open {
        Some(true) => "open",
        Some(false) => "closed",
        None => "unknown",
    };
    let last = summary.last_event.as_deref().unwrap_or("none");
    let amendment = if summary.amendment_pending {
        "; amendment pending"
    } else {
        ""
    };
    format!(
        "  pump      verdict {}; r{}; {total} units: {rendered}\n  pump      complete {complete}{amendment}; registry {registry}; last {last}\n",
        summary.verdict, summary.revision
    )
}

/// One boot block.
#[must_use]
pub fn render_state(summary: &StateSummary, complete_but_open: usize) -> String {
    let rendered = render_counts(&summary.units);
    let activity = if summary.inflight > 0 {
        "working"
    } else {
        "idle"
    };
    format!(
        "  pump      {activity}; units {rendered}\n  pump      complete {}; complete-but-open {complete_but_open}; amendments {}; stuck {}; exceptions {}\n",
        summary.complete, summary.amendments, summary.stuck, summary.exceptions
    )
}

/// `status=count` in sorted order, or `none` for an empty map.
fn render_counts(counts: &BTreeMap<String, usize>) -> String {
    if counts.is_empty() {
        return String::from("none");
    }
    counts
        .iter()
        .map(|(status, count)| format!("{status}={count}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A need is complete when its verdict says delivered, or when it has units
/// and every one is satisfied. A parked verdict is not complete.
fn need_complete(state: &State, handle: &str) -> bool {
    let Some(record) = state.needs.get(handle) else {
        return false;
    };
    if record.verdict == "delivered" {
        return true;
    }
    !record.units.is_empty()
        && record.units.iter().all(|unit| {
            state
                .units
                .get(unit)
                .is_some_and(|unit| satisfied(state, unit, 0))
        })
}

/// `merged` and `no-defect` are satisfied; `replaced` delegates to its
/// successors, with the same depth cap the pump uses.
fn satisfied(state: &State, unit: &Unit, depth: usize) -> bool {
    match unit.status {
        Status::Merged | Status::NoDefect => true,
        Status::Replaced if depth < 6 => {
            !unit.replaced_by.is_empty()
                && unit.replaced_by.iter().all(|id| {
                    state
                        .units
                        .get(id)
                        .is_some_and(|next| satisfied(state, next, depth + 1))
                })
        }
        _ => false,
    }
}
