//! Purging an archived project for good: once
//! [`PURGE_DELAY_SECONDS`](super::PURGE_DELAY_SECONDS) have passed since the
//! archive stamp, remove the project's mailspace root and lanes directory,
//! drop every grant on it, and remove the server's own project directory.
//! The steps are ordered so a partially failed purge retries cleanly: the
//! recorded root leaves the host registry first, the trees go next (an
//! already-removed tree counts as removed), the grants are dropped after
//! them, and the server-side directory — the archive record a retry needs
//! to know where the trees were — is removed last. Purging never derives
//! paths from the current `HostConfig`: everything comes from the archive
//! record, so a purge of a project whose name the host now spells
//! differently still removes what was archived.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::project_create::check_name;
use crate::pump::control::project_home;
use crate::pump::run::lock_is_held;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

use crate::server::AppState;

use super::super::super::tools::{ToolError, map_project, req_str};
use super::super::{CREATING, blocking, hosted_entry};

/// Purge one archived project. Not wired into the MCP dispatch by this
/// slice; a later host slice exposes it.
pub(crate) async fn purge_project(state: &AppState, arguments: &Value) -> Result<Value, ToolError> {
    let name = req_str(arguments, "name")?.to_owned();
    let confirm = req_str(arguments, "confirm")?.to_owned();
    if confirm != name {
        return Err(ToolError::new(format!(
            "purging project {name:?} needs a confirmation that repeats its name"
        )));
    }
    check_name(&name).map_err(|err| ToolError::new(err.to_string()))?;
    let state = state.clone();
    blocking(move || {
        // One host-project mutation at a time, the same mutex
        // `create_project` and `delete_project` hold.
        let _one = CREATING.lock().unwrap_or_else(|e| e.into_inner());
        purge_locked(&state, &name)
    })
    .await
}

/// Everything purge does, under the host mutation mutex.
fn purge_locked(state: &AppState, name: &str) -> Result<Value, ToolError> {
    let meta = hosted_entry(state, name)
        .ok_or_else(|| ToolError::new(format!("no project {name:?} on this host")))?;
    let Some(record) = meta.archived.clone() else {
        return Err(ToolError::new(format!("project {name} is not archived")));
    };

    // The ruling: purging requires at least 24 hours after archive, for
    // every caller. The elapsed time reads only the store's own clock.
    let archived_at = DateTime::parse_from_rfc3339(&record.at)
        .map_err(|error| {
            ToolError::new(format!(
                "the archive stamp {:?} on project {} is corrupt: {error}",
                record.at, name
            ))
        })?
        .with_timezone(&Utc);
    let allowed_from = archived_at + chrono::Duration::seconds(super::PURGE_DELAY_SECONDS);
    if state.projects.now() < allowed_from {
        return Err(ToolError::new(format!(
            "project {name} was archived at {}; purge is allowed from {} (24 hours after the \
             archive)",
            record.at,
            allowed_from.to_rfc3339_opts(SecondsFormat::Secs, true)
        )));
    }

    // The recorded root leaves the registry (again: the archive already
    // unregistered it), so nothing serves the mailspace while it goes.
    crate::registry::update(|registry| Ok(registry.remove(&record.root)))
        .map_err(|error| ToolError::new(format!("could not update the host registry: {error}")))?;
    crate::server::hosted::reconcile_registry(&state.projects, &state.identities);

    // A live pump may be mid-tick or mid-job; removing its files under it
    // would break that run. A root without a resolvable pump configuration
    // (already gone, or never a pump project) has no lock to hold.
    if let Ok(home) = project_home(&record.root) {
        let held = lock_is_held(&home).map_err(|error| {
            ToolError::new(format!(
                "could not inspect the pump lock at {}: {error}",
                home.display()
            ))
        })?;
        if held {
            return Err(ToolError::new(format!(
                "a pump still holds the lock at {}; stop the pump service and call purge again",
                home.join("lock").display()
            )));
        }
    }

    remove_tree(&record.root, "project root")?;
    remove_tree(&record.lanes_dir, "lanes directory")?;
    let (ungranted, revoked) = super::drop_project_grants(state, &meta.project_id)?;

    // LAST: this directory holds the archive record a partially failed
    // purge needs to retry, so it only goes once nothing else is left.
    let data_dir = state
        .projects
        .project_dir(&meta.project_id)
        .map_err(map_project)?;
    remove_tree(&data_dir, "project data directory")?;

    Ok(json!({
        "name": name,
        "project_id": meta.project_id,
        "project_root": record.root,
        "lanes_dir": record.lanes_dir,
        "data_dir": data_dir,
        "ungranted": ungranted,
        "keys_revoked": revoked,
    }))
}

/// Remove a whole tree; absent already counts as removed, so a purge that
/// failed part-way through can simply run again.
fn remove_tree(path: &Path, what: &str) -> Result<(), ToolError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ToolError::new(format!(
            "could not remove the {what} {}: {error}",
            path.display()
        ))),
    }
}

#[cfg(test)]
#[path = "purge_test.rs"]
mod tests;
