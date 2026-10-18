//! Sticky in-memory death for a credential file.
//!
//! A provider is marked dead only after a definitive refresh refusal
//! (HTTP 400/401, or no refresh token). Transient token-endpoint failures
//! do not mark it. There is no background refresh: death is checked on
//! the request that needs the provider, and cleared when the file's
//! identity (path + mtime + size) changes or the process restarts.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// How a token-endpoint response should be treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshClass {
    /// Tokens were issued.
    Ok,
    /// 400/401: the grant is dead; do not retry this file.
    Refused,
    /// Network, 429, or 5xx: fail this request only.
    Transient,
}

/// Classify a token-endpoint HTTP status.
#[must_use]
pub fn classify_status(status: reqwest::StatusCode) -> RefreshClass {
    if status.is_success() {
        RefreshClass::Ok
    } else if status.as_u16() == 400 || status.as_u16() == 401 {
        RefreshClass::Refused
    } else {
        RefreshClass::Transient
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileId {
    modified: Option<SystemTime>,
    len: u64,
}

fn identity(path: &Path) -> Option<FileId> {
    let Ok(meta) = fs::metadata(path) else {
        return None;
    };
    #[allow(clippy::manual_ok_err)]
    let modified = match meta.modified() {
        Ok(time) => Some(time),
        Err(_) => None,
    };
    Some(FileId {
        modified,
        len: meta.len(),
    })
}

struct DeadMap {
    by_path: HashMap<PathBuf, FileId>,
}

fn map() -> &'static Mutex<DeadMap> {
    static MAP: OnceLock<Mutex<DeadMap>> = OnceLock::new();
    MAP.get_or_init(|| {
        Mutex::new(DeadMap {
            by_path: HashMap::new(),
        })
    })
}

/// True when this file was marked dead and has not changed since.
#[must_use]
pub fn is_dead(path: &Path) -> bool {
    let Ok(mut guard) = map().lock() else {
        return false;
    };
    let Some(marked) = guard.by_path.get(path).cloned() else {
        return false;
    };
    match identity(path) {
        Some(now) if now == marked => true,
        Some(_) | None => {
            guard.by_path.remove(path);
            false
        }
    }
}

/// Remember that this file's refresh was refused. Logs one WARN the first time.
pub fn mark_dead(path: &Path) {
    let Some(id) = identity(path) else {
        return;
    };
    let Ok(mut guard) = map().lock() else {
        return;
    };
    let first = guard.by_path.get(path) != Some(&id);
    guard.by_path.insert(path.to_path_buf(), id);
    drop(guard);
    if first {
        tracing::warn!(
            path = %path.display(),
            "credential refresh was refused; log in again (kimi / codex login)"
        );
    }
}

/// Fixed 502 body used when a provider is dead.
pub const DEAD_MESSAGE: &str = "credential refresh was refused; log in again (kimi / codex login)";

#[cfg(test)]
#[path = "dead_test.rs"]
mod dead_test;
