//! Config, lock, and log locations for the user-level proxy.

use std::path::{Path, PathBuf};

use crate::VivariumError;

const CONFIG_DIRECTORY: &str = "vivi";
const CONFIG_FILENAME: &str = "proxy.toml";
const CONFIG_ENV: &str = "VIVI_PROXY_CONFIG";

/// Resolve `--config`, then `VIVI_PROXY_CONFIG`, then the platform default.
///
/// # Errors
///
/// Returns an error when the platform config directory cannot be found.
pub fn resolve_config_path(explicit: Option<&Path>) -> Result<PathBuf, VivariumError> {
    if let Some(path) = explicit {
        return Ok(expand_user(path));
    }
    if let Ok(path) = std::env::var(CONFIG_ENV) {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Ok(expand_user(Path::new(trimmed)));
        }
    }
    default_config_path()
}

/// Platform config dir + `vivi/proxy.toml`.
///
/// # Errors
///
/// Returns an error when `dirs::config_dir` is missing.
pub fn default_config_path() -> Result<PathBuf, VivariumError> {
    Ok(config_root()?.join(CONFIG_FILENAME))
}

/// Directory that holds `proxy.toml` and the instance lock.
///
/// # Errors
///
/// Returns an error when `dirs::config_dir` is missing.
pub fn config_root() -> Result<PathBuf, VivariumError> {
    let root = dirs::config_dir().ok_or_else(|| {
        VivariumError::Config("could not find the user's config directory".into())
    })?;
    Ok(root.join(CONFIG_DIRECTORY))
}

/// Single-instance lock next to the resolved config file.
#[must_use]
pub fn lock_path(config_path: &Path) -> PathBuf {
    config_path.with_extension("lock")
}

/// Default log directory: platform cache dir + `vivi/proxy`.
///
/// # Errors
///
/// Returns an error when `dirs::cache_dir` is missing.
pub fn default_log_dir() -> Result<PathBuf, VivariumError> {
    let root = dirs::cache_dir()
        .ok_or_else(|| VivariumError::Config("could not find the user's cache directory".into()))?;
    Ok(root.join(CONFIG_DIRECTORY).join("proxy"))
}

/// Expand a leading `~` to the home directory.
#[must_use]
pub fn expand_user(path: &Path) -> PathBuf {
    let Some(raw) = path.to_str() else {
        return path.to_path_buf();
    };
    expand_user_str(raw)
}

/// Expand a leading `~` in a UTF-8 path string.
#[must_use]
pub fn expand_user_str(raw: &str) -> PathBuf {
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(raw)
}

/// Restrict a directory to the current user (`0700` on Unix).
///
/// # Errors
///
/// Returns an I/O error when the directory cannot be created.
pub fn ensure_private_dir(path: &Path) -> Result<(), VivariumError> {
    std::fs::create_dir_all(path)?;
    set_dir_mode(path);
    Ok(())
}

#[cfg(unix)]
fn set_dir_mode(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = std::fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o700);
        drop(std::fs::set_permissions(path, permissions));
    }
}

#[cfg(not(unix))]
fn set_dir_mode(_path: &Path) {}
