//! Read credential files in place; refresh under an advisory lock.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::jwt;
use crate::VivariumError;

/// Open (or create) a sibling `.lock` and take a blocking exclusive lock.
///
/// # Errors
///
/// Returns an I/O error when the lock file cannot be created or locked.
pub fn lock_path(path: &Path) -> Result<File, VivariumError> {
    let lock = lock_file_path(path);
    if let Some(parent) = lock.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock)?;
    file.lock().map_err(|error| {
        VivariumError::Message(format!("could not lock {}: {error}", lock.display()))
    })?;
    Ok(file)
}

/// Sibling lock path: `auth.json` → `auth.json.lock`.
#[must_use]
pub fn lock_file_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map_or_else(
        || std::ffi::OsString::from("credentials"),
        std::ffi::OsStr::to_os_string,
    );
    name.push(".lock");
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

/// Read a credential JSON object. Missing file is a configuration error.
///
/// # Errors
///
/// Returns a configuration error when the file is missing or not JSON.
pub fn read_json(path: &Path) -> Result<serde_json::Value, VivariumError> {
    let text = fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            VivariumError::Config(format!("credential file not found: {}", path.display()))
        } else {
            error.into()
        }
    })?;
    serde_json::from_str(&text).map_err(|error| {
        VivariumError::Config(format!(
            "invalid credential JSON {}: {error}",
            path.display()
        ))
    })
}

/// Atomically replace `path` with `value`, mode `0600`.
///
/// # Errors
///
/// Returns an I/O error when the temporary file cannot be written or renamed.
pub fn write_json_atomic(path: &Path, value: &serde_json::Value) -> Result<(), VivariumError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| VivariumError::Config(format!("could not encode credentials: {error}")))?;
    write_atomic(path, text.as_bytes())
}

/// Atomically replace `path` with `bytes`, mode `0600`.
///
/// # Errors
///
/// Returns an I/O error when the temporary file cannot be written or renamed.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), VivariumError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = temp_path(path);
    write_private(&temporary, bytes)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map_or_else(
        || std::ffi::OsString::from("credentials"),
        std::ffi::OsStr::to_os_string,
    );
    name.push(".tmp");
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), VivariumError> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    Ok(())
}

/// First existing path in `candidates`.
///
/// # Errors
///
/// Returns a configuration error when none of the paths exist.
pub fn first_existing(candidates: &[PathBuf]) -> Result<PathBuf, VivariumError> {
    for path in candidates {
        if path.is_file() {
            return Ok(path.clone());
        }
    }
    let listed = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(VivariumError::Config(format!(
        "no credential file found (tried {listed})"
    )))
}

/// Expiry instant from a JWT or an `expires_at` unix timestamp. Never the token.
#[must_use]
pub fn expiry_label(value: &serde_json::Value) -> Option<String> {
    if let Some(exp) = jwt_exp_from_value(value) {
        return Some(chrono::DateTime::from_timestamp(exp, 0)?.to_rfc3339());
    }
    if let Some(exp) = value.get("expires_at").and_then(as_unix) {
        return Some(chrono::DateTime::from_timestamp(exp, 0)?.to_rfc3339());
    }
    None
}

fn jwt_exp_from_value(value: &serde_json::Value) -> Option<i64> {
    let token = value
        .pointer("/tokens/access_token")
        .or_else(|| value.get("access_token"))
        .and_then(serde_json::Value::as_str)?;
    jwt::exp_unix(token)
}

fn as_unix(value: &serde_json::Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        value.as_u64().and_then(|number| {
            #[allow(clippy::manual_ok_err)]
            match i64::try_from(number) {
                Ok(value) => Some(value),
                Err(_) => None,
            }
        })
    })
}
