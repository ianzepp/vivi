//! Blocked-scope resolution: a deterministic check of a hand's request for
//! paths outside its slice's write scope against the scopes of the run's
//! other open slices. No model is involved.

use crate::error::VivariumError;
use crate::pump::flow::fanout::SliceEdges;
use crate::pump::store::PumpStore;

/// A request for more paths than this goes to the slicer.
const MAX_REQUEST_PATHS: usize = 5;

/// Where a slice stands in blocked-scope resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeState {
    /// No blocked-scope request is outstanding.
    Clear,
    /// Waiting for slices that hold the requested paths to close.
    Wait,
    /// Waiting for the slicer to repair the scopes.
    Repair,
}

impl ScopeState {
    /// The word stored in `slice_scope.scope_state`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Clear => "none",
            Self::Wait => "scope-wait",
            Self::Repair => "scope-repair",
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
            "scope-repair" => Self::Repair,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// No slice holds the paths: they were added to the slice's scope.
    Widen(Vec<String>),
    /// These slice tasks hold some of the paths; the slice now depends on them.
    WaitOn(Vec<String>),
    /// The slicer must repair the scopes; the reason says why.
    Repair(String),
}

/// Marks a requested-but-held path in the stored wait record (the
/// `slice_scope.holders` list); holder task handles never start with it.
const HELD_PATH_PREFIX: &str = "path:";

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

/// True when a scope entry holds `path`: the same path, or a directory entry
/// (ending in `/`) that is a prefix of it.
fn holds(entry: &str, path: &str) -> bool {
    entry == path || (entry.ends_with('/') && path.starts_with(entry))
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

/// Settle a blocked-scope request from `task`, a slice of `run`.
///
/// # Errors
/// Returns [`VivariumError`] when the store or the mailspace fails.
pub fn resolve(
    store: &PumpStore,
    mail: &mut impl SliceEdges,
    run: u32,
    task: &str,
    req: &BlockedScope,
) -> Result<Resolution, VivariumError> {
    let requests = store.bump_scope_requests(task)?;
    if requests >= 2 {
        return Ok(Resolution::Repair(format!(
            "second blocked-scope request on {task}"
        )));
    }
    if req.paths.len() > MAX_REQUEST_PATHS {
        return Ok(Resolution::Repair(format!(
            "{} paths requested; the limit is {MAX_REQUEST_PATHS}",
            req.paths.len()
        )));
    }
    if req.design {
        return Ok(Resolution::Repair(format!(
            "design question: {}",
            req.reason
        )));
    }
    let others = store.open_scopes_of_run(task, run)?;
    let Split {
        holders,
        free,
        held_paths,
    } = split_request(&req.paths, &others);
    if !free.is_empty() {
        store.widen_slice_scope(task, &free)?;
    }
    if holders.is_empty() {
        return Ok(Resolution::Widen(free));
    }
    for holder in &holders {
        mail.add_dependency(task, holder)?;
    }
    let mut record = holders.clone();
    record.extend(
        held_paths
            .iter()
            .map(|path| format!("{HELD_PATH_PREFIX}{path}")),
    );
    store.set_scope_holders(task, &record)?;
    store.set_scope_state(task, ScopeState::Wait)?;
    Ok(Resolution::WaitOn(holders))
}

#[cfg(test)]
#[path = "scope_test.rs"]
mod tests;
