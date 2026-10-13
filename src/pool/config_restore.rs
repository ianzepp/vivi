//! The pool's `restore` setting: tracked file paths the runner puts back to
//! the start commit before a job commits.

use super::config::PoolConfig;
use crate::error::VivariumError;

/// The most paths a pool's `restore` list may hold.
pub const MAX_RESTORE_PATHS: usize = 100;

impl PoolConfig {
    /// Check the pool's `restore` setting: a list of repository-relative file
    /// paths, each non-empty, without a leading `/`, without a `..` component,
    /// and without a newline or any other control character.
    ///
    /// # Errors
    /// Returns a configuration error naming `[pools.<pool>] restore` when a
    /// path is malformed or the list is longer than [`MAX_RESTORE_PATHS`].
    pub fn check_restore(&self, pool: &str) -> Result<(), VivariumError> {
        if self.restore.len() > MAX_RESTORE_PATHS {
            return Err(VivariumError::Config(format!(
                "[pools.{pool}] restore holds {} paths; the limit is {MAX_RESTORE_PATHS}",
                self.restore.len()
            )));
        }
        for path in &self.restore {
            check_restore_path(pool, path)?;
        }
        Ok(())
    }
}

fn check_restore_path(pool: &str, path: &str) -> Result<(), VivariumError> {
    let refused = |why: String| {
        VivariumError::Config(format!("[pools.{pool}] restore: {why}: {path:?}"))
    };
    if path.is_empty() {
        return Err(refused("a path is empty".into()));
    }
    if path.starts_with('/') {
        return Err(refused(
            "a path must be relative to the repository (no leading /)".into(),
        ));
    }
    if path.split('/').any(|component| component == "..") {
        return Err(refused("a path must not contain a .. component".into()));
    }
    if path.chars().any(char::is_control) {
        return Err(refused(
            "a path must not hold a newline or other control character".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "config_restore_test.rs"]
mod tests;
