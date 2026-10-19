//! The 2x build-output cap, slot marks, and free-disk measurements.
//!
//! A slot's build directory is disposable. After a fresh seed (or the first
//! observed post-job size) its byte size is stored as `marks/build_baseline`.
//! Later growth past twice that baseline is reclaimed. A slot that predates
//! the mark is held to [`DEFAULT_BASELINE_BYTES`] so a 90 GiB leftover cannot
//! become its own baseline.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::error::VivariumError;

/// Reclaim when `bytes > BUILD_CAP_MUL * baseline`.
pub const BUILD_CAP_MUL: u64 = 2;
/// Free space (GiB) at which dispatch first reclaims free slots.
pub const DISK_RECLAIM_GIB: u64 = 20;
/// Free space (GiB) at which dispatch refuses after reclaim.
pub const DISK_REFUSE_GIB: u64 = 10;
/// Implied baseline when a slot has no mark (about a fresh Rust build).
pub const DEFAULT_BASELINE_BYTES: u64 = 4 * GIB;
/// 2<sup>30</sup> bytes.
pub const GIB: u64 = 1024 * 1024 * 1024;
/// 2<sup>10</sup> bytes.
pub const KIB: u64 = 1024;

/// `marks/build_baseline`: decimal byte size after a fresh build.
pub const MARK_BASELINE: &str = "build_baseline";
/// `marks/last_task`: the task that last claimed the slot.
pub const MARK_LAST_TASK: &str = "last_task";
/// `marks/last_branch`: the job branch of that claim, when known.
pub const MARK_LAST_BRANCH: &str = "last_branch";

/// True when `bytes` is more than twice `baseline`.
#[must_use]
pub fn over_cap(bytes: u64, baseline: u64) -> bool {
    baseline > 0 && bytes > BUILD_CAP_MUL.saturating_mul(baseline)
}

/// The stored baseline, else the pool's configured default, else
/// [`DEFAULT_BASELINE_BYTES`].
#[must_use]
pub fn effective_baseline(stored: Option<u64>, pool_default: Option<u64>) -> u64 {
    stored.or(pool_default).unwrap_or(DEFAULT_BASELINE_BYTES)
}

/// Byte size of `path` (files only). Symlinks are not followed or counted.
#[must_use]
pub fn dir_bytes(path: &Path) -> u64 {
    let mut total: u64 = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let child = entry.path();
            let Ok(meta) = fs::symlink_metadata(&child) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(child);
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

/// Delete the contents of `path`, not `path` itself. Returns the bytes that
/// were there. Symlink children are unlinked, never followed.
///
/// # Errors
/// Returns an error if a child cannot be removed.
pub fn empty_dir(path: &Path) -> Result<u64, VivariumError> {
    if !path.exists() {
        return Ok(0);
    }
    let bytes = dir_bytes(path);
    for entry in fs::read_dir(path)? {
        let child = entry?.path();
        let meta = fs::symlink_metadata(&child)?;
        if meta.file_type().is_symlink() || meta.is_file() {
            fs::remove_file(&child)?;
        } else {
            fs::remove_dir_all(&child)?;
        }
    }
    Ok(bytes)
}

/// The first line of `slot/marks/<name>`, when the file exists.
#[must_use]
pub fn read_mark(slot: &Path, name: &str) -> Option<String> {
    let text = match fs::read_to_string(slot.join("marks").join(name)) {
        Ok(text) => text,
        Err(_) => return None,
    };
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Write `value` as the single line of `slot/marks/<name>`.
///
/// # Errors
/// Returns an error if the marks directory or file cannot be written.
pub fn write_mark(slot: &Path, name: &str, value: &str) -> Result<(), VivariumError> {
    let path = slot.join("marks").join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{value}\n"))?;
    Ok(())
}

/// The stored baseline in bytes.
#[must_use]
pub fn read_baseline(slot: &Path) -> Option<u64> {
    match read_mark(slot, MARK_BASELINE)?.parse() {
        Ok(bytes) => Some(bytes),
        Err(_) => None,
    }
}

/// Record `bytes` as the slot's post-fresh-build baseline.
///
/// # Errors
/// Returns an error if the mark cannot be written.
pub fn write_baseline(slot: &Path, bytes: u64) -> Result<(), VivariumError> {
    write_mark(slot, MARK_BASELINE, &bytes.to_string())
}

/// Free bytes on the volume that holds `path`, from `df -Pk`.
#[must_use]
pub fn volume_free_bytes(path: &Path) -> Option<u64> {
    let out = match Command::new("df").arg("-Pk").arg(path).output() {
        Ok(out) => out,
        Err(_) => return None,
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let field = text.lines().nth(1)?.split_whitespace().nth(3)?;
    match field.parse::<u64>() {
        Ok(kib) => Some(kib.saturating_mul(KIB)),
        Err(_) => None,
    }
}

/// A short `12.4 GiB` / `800 MiB` / `12 KiB` rendering.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{} KiB", bytes / KIB)
    }
}

/// `target` plus a configured relative build directory, existing ones only.
#[must_use]
pub fn build_paths(checkout: &Path, configured: Option<&str>) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    if let Some(name) = configured.filter(|n| !n.is_empty()) {
        paths.push(checkout.join(name));
    }
    let cargo = checkout.join("target");
    if !paths.iter().any(|p| p == &cargo) {
        paths.push(cargo);
    }
    paths.into_iter().filter(|p| p.is_dir()).collect()
}

#[cfg(test)]
#[path = "clean_rule_test.rs"]
mod tests;
