//! Blocked-scope resolution: a deterministic check of a hand's request for
//! paths outside its slice's write scope against the scopes of the run's
//! other open slices. No model is involved.

use serde::{Deserialize, Serialize};

use crate::error::VivariumError;
use crate::pump::flow::fanout::{self, SliceEdges};
use crate::pump::store::{
    PumpStore, ScopeCoordinationResult, ScopeCoordinationSnapshot, ScopeRequest,
};

/// A request for more paths than this goes to the slicer.
const MAX_REQUEST_PATHS: usize = 5;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedScope {
    pub paths: Vec<String>,
    pub reason: String,
    /// The need is a design question, not a missing path.
    pub design: bool,
}

/// How the request was settled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Resolution {
    /// No slice holds the paths: they were added to the slice's scope.
    Widen(Vec<String>),
    /// These slice tasks hold some of the paths; the slice now depends on them.
    WaitOn(Vec<String>),
    /// The slicer must rework the scopes; the reason says why.
    Rework(String),
}

/// Marks a requested-but-held path in the stored wait record (the
/// `slice_scope.holders` list); holder task handles never start with it.
pub const HELD_PATH_PREFIX: &str = "path:";

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

/// True when a scope entry and a requested path overlap: the same path, or
/// a directory entry (ending in `/`) that contains the other. Directory
/// prefixes are component-aware (`src/pump/` never covers
/// `src/pump-other/`); a file entry holds only itself.
fn holds(entry: &str, path: &str) -> bool {
    entry == path
        || (entry.ends_with('/') && path.starts_with(entry))
        || (path.ends_with('/') && entry.starts_with(path))
}

/// A request split by who holds its paths.
struct Split {
    /// The open slices that hold some requested path.
    holders: Vec<String>,
    /// Requested paths no open slice holds.
    free: Vec<String>,
    /// Requested paths some open slice holds.
    held_paths: Vec<String>,
}

fn split_request(paths: &[String], others: &[(String, Vec<String>)]) -> Split {
    let mut split = Split {
        holders: Vec::new(),
        free: Vec::new(),
        held_paths: Vec::new(),
    };
    for path in paths {
        let mut held = false;
        for (other, scope) in others {
            if scope.iter().any(|entry| holds(entry, path)) {
                held = true;
                if !split.holders.contains(other) {
                    split.holders.push(other.clone());
                }
            }
        }
        if held {
            split.held_paths.push(path.clone());
        } else {
            split.free.push(path.clone());
        }
    }
    split
}

/// Settle the blocked-scope request `req` from `request.task`, a slice of
/// run `request.run`, under the journal identity `request` (keyed by the
/// asking job's original source id).
///
/// The first call for a source settles and records the coordination: the
/// request counter, the holders record, the scope state and the journal
/// entry move in one transaction; a wait's free paths are widened just
/// before it, idempotently, because the typed commit carries no free half
/// for a wait. A replay of the same source adopts the recorded resolution —
/// no second request is counted, no holder is recomputed against changed
/// scheduling state, nothing is re-widened — and only the external
/// dependency writes run again, from the recorded holder set, so a failed
/// mail write is recoverable.
///
/// # Errors
/// Returns [`VivariumError`] when the store, the journal or the mail fails.
pub fn resolve(
    store: &PumpStore,
    mail: &mut impl SliceEdges,
    request: &ScopeRequest,
    req: &BlockedScope,
) -> Result<Resolution, VivariumError> {
    store.prepare_scope_request(request)?;
    let settled = store
        .scope_request(&request.source)?
        .is_some_and(|row| row.coordination.is_some());
    let snapshot = store.scope_coordination_snapshot(&request.task, request.run)?;
    let candidate = candidate_for(&snapshot, &request.task, req);
    if !settled && !candidate.free.is_empty() {
        store.widen_slice_scope(&request.task, &candidate.free)?;
    }
    let result = store
        .commit_scope_coordination(request, &candidate.result)?
        .resolution()
        .clone();
    if let Resolution::WaitOn(holders) = &result.resolution {
        fanout::wait_on(mail, &request.task, holders)?;
    }
    Ok(result.resolution)
}

/// What a fresh source settles into under `snapshot`'s scheduling state:
/// the settled rules (a second request, more than [`MAX_REQUEST_PATHS`]
/// paths, or a design question go to the slicer), else the free/held split
/// of the requested paths. Committing `result` persists it; a replay
/// discards it for the recorded resolution.
fn candidate_for(
    snapshot: &ScopeCoordinationSnapshot,
    task: &str,
    req: &BlockedScope,
) -> Candidate {
    let rework = |reason: String| Candidate {
        result: ScopeCoordinationResult {
            resolution: Resolution::Rework(reason),
            held: Vec::new(),
        },
        free: Vec::new(),
    };
    if snapshot.requests >= 1 {
        return rework(format!("second blocked-scope request on {task}"));
    }
    if req.paths.len() > MAX_REQUEST_PATHS {
        return rework(format!(
            "{} paths requested; the limit is {MAX_REQUEST_PATHS}",
            req.paths.len()
        ));
    }
    if req.design {
        return rework(format!("design question: {}", req.reason));
    }
    let Split {
        holders,
        free,
        held_paths,
    } = split_request(&req.paths, &snapshot.others);
    if holders.is_empty() {
        return Candidate {
            result: ScopeCoordinationResult {
                resolution: Resolution::Widen(free),
                held: Vec::new(),
            },
            free: Vec::new(),
        };
    }
    Candidate {
        result: ScopeCoordinationResult {
            resolution: Resolution::WaitOn(holders),
            held: held_paths,
        },
        free,
    }
}

/// A computed coordination and the free paths a wait must widen before its
/// commit (the typed commit carries no free half for a wait).
struct Candidate {
    result: ScopeCoordinationResult,
    free: Vec<String>,
}

#[cfg(test)]
#[path = "scope_test.rs"]
mod tests;
