//! Blocked-scope resolution: a deterministic check of a hand's request for
//! paths outside its slice's write scope against the scopes of the run's
//! other open slices. No model is involved.

use crate::error::VivariumError;
use crate::pump::flow::mail::MailspacePort;
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

/// The mailspace write `resolve` needs: a dependency edge between tasks.
pub trait ScopeMail {
    /// Make `task` depend on `depends_on`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the edge cannot be written.
    fn add_dependency(&mut self, task: &str, depends_on: &str) -> Result<(), VivariumError>;
}

impl ScopeMail for MailspacePort {
    fn add_dependency(&mut self, task: &str, depends_on: &str) -> Result<(), VivariumError> {
        self.mailspace
            .set_dependencies(
                &self.pump_identity,
                task,
                "task",
                &[depends_on.to_string()],
                &[],
            )
            .map(|_| ())
    }
}

/// True when a scope entry holds `path`: the same path, or a directory entry
/// (ending in `/`) that is a prefix of it.
fn holds(entry: &str, path: &str) -> bool {
    entry == path || (entry.ends_with('/') && path.starts_with(entry))
}

/// Settle a blocked-scope request from `task`, a slice of `run`.
///
/// # Errors
/// Returns [`VivariumError`] when the store or the mailspace fails.
pub fn resolve(
    store: &PumpStore,
    mail: &mut impl ScopeMail,
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
    let mut holders: Vec<String> = Vec::new();
    let mut free: Vec<String> = Vec::new();
    for path in &req.paths {
        let mut held = false;
        for (other, scope) in &others {
            if scope.iter().any(|entry| holds(entry, path)) {
                held = true;
                if !holders.contains(other) {
                    holders.push(other.clone());
                }
            }
        }
        if !held {
            free.push(path.clone());
        }
    }
    if !free.is_empty() {
        store.widen_slice_scope(task, &free)?;
    }
    if holders.is_empty() {
        return Ok(Resolution::Widen(free));
    }
    for holder in &holders {
        mail.add_dependency(task, holder)?;
    }
    store.set_scope_holders(task, &holders)?;
    store.set_scope_state(task, ScopeState::Wait)?;
    Ok(Resolution::WaitOn(holders))
}

#[cfg(test)]
impl ScopeMail for crate::pump::flow::fake_mail::FakeMail {
    fn add_dependency(&mut self, task: &str, depends_on: &str) -> Result<(), VivariumError> {
        let entry = self
            .tasks
            .get_mut(task)
            .ok_or_else(|| VivariumError::Message(format!("fake mail: no task {task}")))?;
        entry.depends_on.push(depends_on.to_string());
        Ok(())
    }
}

#[cfg(test)]
#[path = "scope_test.rs"]
mod tests;
