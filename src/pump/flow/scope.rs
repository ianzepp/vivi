//! Blocked-scope resolution: a deterministic check of a hand's request for
//! paths outside its slice's write scope against the scopes of the run's
//! other open slices. No model is involved. The settlement itself is the
//! store's, keyed by the request's originating source job
//! ([`PumpStore::apply_scope_resolution`]), so a replayed answer is free.

use crate::error::VivariumError;
use crate::pump::flow::fanout::{self, SliceEdges};
use crate::pump::store::PumpStore;
use crate::pump::store::scope_resolution::HELD_PATH_PREFIX;

/// Where a slice stands in blocked-scope resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeState {
    /// No blocked-scope request is outstanding.
    Clear,
    /// Waiting for slices that hold the requested paths to close.
    Wait,
    /// Waiting for the slicer to rework the scopes.
    Rework,
}

impl ScopeState {
    /// The word stored in `slice_scope.scope_state`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Clear => "none",
            Self::Wait => "scope-wait",
            Self::Rework => "scope-rework",
        }
    }

    /// Read a stored word.
    ///
    /// # Errors
    /// Returns [`VivariumError`] for a word that is not a scope state.
    pub fn parse(word: &str) -> Result<Self, VivariumError> {
        Ok(match word {
            "none" => Self::Clear,
            "scope-wait" => Self::Wait,
            "scope-rework" => Self::Rework,
            other => {
                return Err(VivariumError::Other(format!(
                    "pump.db slice_scope: unknown scope state {other:?}"
                )));
            }
        })
    }
}

/// What a hand asked for when it answered `blocked-scope`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BlockedScope {
    pub paths: Vec<String>,
    pub reason: String,
    /// The need is a design question, not a missing path.
    pub design: bool,
}

/// How the request was settled.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Resolution {
    /// No slice holds the paths: they were added to the slice's scope.
    Widen(Vec<String>),
    /// These slice tasks hold some of the paths; the slice now depends on them.
    WaitOn(Vec<String>),
    /// The slicer must rework the scopes; the reason says why.
    Rework(String),
}

/// The slice tasks a waiting slice is waiting on.
///
/// # Errors
/// Returns [`VivariumError`] when the store fails.
pub fn wait_holders(store: &PumpStore, task: &str) -> Result<Vec<String>, VivariumError> {
    Ok(store
        .scope_holders(task)?
        .into_iter()
        .filter(|entry| !entry.starts_with(HELD_PATH_PREFIX))
        .collect())
}

/// The holders landed: add the paths the hand asked for and found held to the
/// slice's write scope, so the re-dispatched brief grants them.
///
/// # Errors
/// Returns [`VivariumError`] when the store fails.
pub fn grant_held(store: &PumpStore, task: &str) -> Result<(), VivariumError> {
    let held: Vec<String> = store
        .scope_holders(task)?
        .into_iter()
        .filter_map(|entry| entry.strip_prefix(HELD_PATH_PREFIX).map(str::to_string))
        .collect();
    if held.is_empty() {
        return Ok(());
    }
    store.widen_slice_scope(task, &held)
}

/// The wait is over and the slice went out again: forget the wait record. The
/// request that ended in the wait still counts, so the slice's next blocked
/// request is its second.
///
/// # Errors
/// Returns [`VivariumError`] when the store fails.
pub fn finish_grant(store: &PumpStore, task: &str) -> Result<(), VivariumError> {
    store.set_scope_holders(task, &[])
}

/// Settle a blocked-scope request from `task`, a slice of `run`, keyed by the
/// request's originating source job. A source the store has already settled
/// returns its stored resolution: no new request count, no recomputation
/// against the scopes as they stand now. The holder dependency edges are
/// (re-)written after the store commit; the mailspace dependency write is
/// idempotent, so a replayed settlement cannot duplicate an edge.
///
/// # Errors
/// Returns [`VivariumError`] when the store or the mailspace fails.
pub fn resolve(
    store: &PumpStore,
    mail: &mut impl SliceEdges,
    run: u32,
    task: &str,
    source_job: &str,
    req: &BlockedScope,
) -> Result<Resolution, VivariumError> {
    let record = store.apply_scope_resolution(source_job, task, run, req)?;
    fanout::wait_on(mail, task, &record.holders)?;
    Ok(record.resolution)
}

#[cfg(test)]
#[path = "scope_test.rs"]
mod tests;
