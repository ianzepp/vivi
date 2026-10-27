//! Project-local goal path registry for Mind/campaign orientation.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use super::Mailspace;
use crate::storage::GoalRow;
use vivi_error::VivariumError;

/// Goal registration plus whether the path currently exists on disk.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GoalView {
    pub handle: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registered_by: Option<String>,
    pub created_at: String,
    pub exists: bool,
}

impl Mailspace {
    /// Register a goal document path. Content stays in the file; Vivi stores only the pointer.
    ///
    /// # Errors
    /// Returns an error when the path is invalid, missing, outside the project, or already registered.
    pub fn goal_add(
        &self,
        path: &Path,
        label: Option<&str>,
        registered_by: Option<&str>,
    ) -> Result<GoalView, VivariumError> {
        let relative = normalize_goal_path(&self.root, path)?;
        let absolute = self.root.join(&relative);
        if !absolute.is_file() {
            return Err(VivariumError::Other(format!(
                "goal path is not a file: {}",
                absolute.display()
            )));
        }
        let row = self
            .storage()?
            .goal_insert(&relative, label, registered_by)?;
        Ok(goal_view(&self.root, row))
    }

    /// List registered goals with live existence checks.
    ///
    /// # Errors
    /// Returns an error when storage cannot be opened or listed.
    pub fn goal_list(&self) -> Result<Vec<GoalView>, VivariumError> {
        let rows = self.storage()?.goals()?;
        Ok(rows
            .into_iter()
            .map(|row| goal_view(&self.root, row))
            .collect())
    }

    /// Show one goal by handle, handle prefix, or path.
    ///
    /// # Errors
    /// Returns an error when the selector is missing or ambiguous.
    pub fn goal_show(&self, selector: &str) -> Result<GoalView, VivariumError> {
        let row = self.resolve_goal(selector)?;
        Ok(goal_view(&self.root, row))
    }

    /// Remove a goal registration. Does not delete the file.
    ///
    /// # Errors
    /// Returns an error when the selector is missing or the delete fails.
    pub fn goal_drop(&self, selector: &str) -> Result<GoalView, VivariumError> {
        let row = self.resolve_goal(selector)?;
        let view = goal_view(&self.root, row.clone());
        if !self.storage()?.goal_delete(&row.handle)? {
            return Err(VivariumError::Other(format!(
                "goal not found: {}",
                row.handle
            )));
        }
        Ok(view)
    }

    fn resolve_goal(&self, selector: &str) -> Result<GoalRow, VivariumError> {
        let storage = self.storage()?;
        if let Some(row) = storage.goal_by_handle(selector)? {
            return Ok(row);
        }
        if selector.starts_with("gol_")
            && let Some(row) = storage.goal_by_handle_prefix(selector)?
        {
            return Ok(row);
        }
        if let Ok(relative) = normalize_goal_path(&self.root, Path::new(selector))
            && let Some(row) = storage.goal_by_path(&relative)?
        {
            return Ok(row);
        }
        if let Some(row) = storage.goal_by_path(selector)? {
            return Ok(row);
        }
        Err(VivariumError::Other(format!("goal not found: {selector}")))
    }
}

fn goal_view(root: &Path, row: GoalRow) -> GoalView {
    let exists = root.join(&row.path).is_file();
    GoalView {
        handle: row.handle,
        path: row.path,
        label: row.label,
        registered_by: row.registered_by,
        created_at: row.created_at,
        exists,
    }
}

/// Normalize a goal path to a project-relative POSIX-style string under `root`.
fn normalize_goal_path(root: &Path, path: &Path) -> Result<String, VivariumError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let root_canon = canonicalize_or(root)?;
    let abs_canon = if absolute.exists() {
        canonicalize_or(&absolute)?
    } else {
        // Allow missing parents only for resolve-by-path after delete of file;
        // add still requires is_file after join.
        let parent = absolute.parent().unwrap_or(Path::new("."));
        let file_name = absolute
            .file_name()
            .ok_or_else(|| VivariumError::Other("goal path has no file name".into()))?;
        if parent.as_os_str().is_empty() || parent == Path::new("") {
            root_canon.join(file_name)
        } else if parent.exists() {
            canonicalize_or(parent)?.join(file_name)
        } else {
            absolute
        }
    };
    let relative = abs_canon.strip_prefix(&root_canon).map_err(|_| {
        VivariumError::Other(format!(
            "goal path must be inside project root {}: {}",
            root.display(),
            path.display()
        ))
    })?;
    if relative.as_os_str().is_empty() {
        return Err(VivariumError::Other(
            "goal path must be a file inside the project, not the project root".into(),
        ));
    }
    if relative
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(VivariumError::Other(
            "goal path must not escape the project root".into(),
        ));
    }
    Ok(relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

fn canonicalize_or(path: &Path) -> Result<PathBuf, VivariumError> {
    path.canonicalize().map_err(|e| {
        VivariumError::Other(format!("failed to resolve path {}: {e}", path.display()))
    })
}

#[cfg(test)]
#[path = "goals_test.rs"]
mod tests;
