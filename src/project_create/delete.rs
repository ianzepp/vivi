//! Delete a project from a host.
//!
//! [`delete`] takes project `<name>` off the host registry, then removes
//! its root `<projects_root>/<name>` and its lanes directory
//! `<lanes_root>/<name>` with every lane in it. The pump service and a
//! server stop running and serving a project once its registration is gone,
//! so the registration goes first; the files go only once no pump holds the
//! project's lock. While one still does, the call stops after unregistering
//! and says so: run it again once that pump has exited. A repeated call
//! finishes what an earlier one left, so a delete can always be completed.
//!
//! The confirmation must repeat the name: this removes the project's
//! mailspace and every lane's history on this host.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{check_name, config, existing_dir};
use crate::VivariumError;
use crate::pump::control;
use crate::pump::run::lock_is_held;
use crate::registry;

/// Everything needed to delete one project.
#[derive(Clone, Debug)]
pub struct ProjectDelete {
    /// Project name, as given to `create`.
    pub name: String,
    /// Directory that holds project roots.
    pub projects_root: PathBuf,
    /// Directory that holds each project's lanes directory.
    pub lanes_root: PathBuf,
    /// Must equal `name`.
    pub confirm: String,
}

/// What [`delete`] removed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProjectDeleted {
    pub name: String,
    /// The registration was removed by this call.
    pub unregistered: bool,
    /// The project root, when this call removed it.
    pub project_root: Option<PathBuf>,
    /// The lanes directory, when this call removed it.
    pub lanes_dir: Option<PathBuf>,
}

/// Delete a project registered in the host registry (`VIVI_CONFIG_DIR` or
/// the user config directory).
///
/// # Errors
/// Fails on an invalid name, a confirmation that does not repeat it, a
/// project that is not on this host at all, a pump that still holds the
/// project's lock (after unregistering), or a removal failing.
pub fn delete(request: &ProjectDelete) -> Result<ProjectDeleted, VivariumError> {
    delete_at(request, &registry::path()?)
}

/// [`delete`] against an explicit registry file.
///
/// # Errors
/// As [`delete`].
pub fn delete_at(
    request: &ProjectDelete,
    registry_file: &Path,
) -> Result<ProjectDeleted, VivariumError> {
    check_name(&request.name)?;
    if request.confirm != request.name {
        return Err(config(&format!(
            "deleting project {:?} needs a confirmation that repeats its name",
            request.name
        )));
    }
    let root = existing_dir(&request.projects_root, "projects root")?.join(&request.name);
    let lanes = existing_dir(&request.lanes_root, "lanes root")?.join(&request.name);
    let unregistered = registry::update_at(registry_file, |registry| Ok(registry.remove(&root)))?;
    let root_exists = root.symlink_metadata().is_ok();
    let lanes_exist = lanes.symlink_metadata().is_ok();
    if !unregistered && !root_exists && !lanes_exist {
        return Err(config(&format!(
            "no project {:?} on this host: not registered, and neither {} nor {} exists",
            request.name,
            root.display(),
            lanes.display()
        )));
    }
    if root_exists {
        refuse_live_pump(&request.name, &root)?;
    }
    Ok(ProjectDeleted {
        name: request.name.clone(),
        unregistered,
        project_root: remove(&root, root_exists)?,
        lanes_dir: remove(&lanes, lanes_exist)?,
    })
}

/// A pump that still owns the project's lock may be mid-tick or mid-job;
/// removing its files under it would break that run, so wait for it.
fn refuse_live_pump(name: &str, root: &Path) -> Result<(), VivariumError> {
    let Ok(home) = control::project_home(root) else {
        return Ok(());
    };
    if lock_is_held(&home)? {
        return Err(VivariumError::Message(format!(
            "project {name:?} is unregistered, but a pump still holds its lock; the pump \
             service stops it within a round. Run the delete again once it has exited"
        )));
    }
    Ok(())
}

fn remove(path: &Path, exists: bool) -> Result<Option<PathBuf>, VivariumError> {
    if !exists {
        return Ok(None);
    }
    fs::remove_dir_all(path).map_err(|error| {
        VivariumError::Other(format!("could not remove {}: {error}", path.display()))
    })?;
    Ok(Some(path.to_path_buf()))
}

#[cfg(test)]
#[path = "delete_test.rs"]
mod tests;
