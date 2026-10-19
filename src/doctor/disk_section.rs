//! The disk section of `vivi doctor`: what the growth paths weigh.

use std::path::Path;

use super::Finding;
use crate::mailspace::Mailspace;
use crate::pool::config::PoolConfig;

const SECTION: &str = "disk";
/// Bytes per GiB, for human sizes.
const BYTES_PER_GIB: u64 = 1024 * 1024 * 1024;
/// A pool root warns above this size.
const POOL_ROOT_WARN_GIB: u64 = 20;
/// The pump home (logs, plans, gate results, the store) warns above this.
const PUMP_HOME_WARN_GIB: u64 = 2;
/// The integration clone warns above this size.
const CLONE_WARN_GIB: u64 = 10;
/// More live `unit/*` branches than this means closeout is falling behind.
const BRANCHES_WARN: usize = 400;
/// The fix a bloated path points at.
const RECLAIM_FIX: &str =
    "reclaim space (vivi pool clean; prune old merge lanes and unit branches)";

/// Report the disk section of `vivi doctor`.
#[must_use]
pub fn findings(project: &Path) -> Vec<Finding> {
    let Ok(space) = Mailspace::discover(Some(project)) else {
        return vec![Finding::skipped(SECTION, "disk", "mailspace cannot be opened")];
    };
    let mut out = Vec::new();
    match crate::pool::pools::configured(&space) {
        Err(_) => out.push(Finding::skipped(SECTION, "pools", "no pools configured")),
        Ok(pools) => {
            for (name, config) in pools {
                out.push(pool_root_finding(project, &space.config.name, name, config));
            }
        }
    }
    let pump = space.config.pump.clone().unwrap_or_default();
    let home = pump.home_dir(project);
    let repo = pump.repo_dir(project);
    out.push(size_finding(
        SECTION,
        "pump home",
        &home,
        PUMP_HOME_WARN_GIB * BYTES_PER_GIB,
    ));
    out.push(size_finding(
        SECTION,
        "integration clone",
        &repo,
        CLONE_WARN_GIB * BYTES_PER_GIB,
    ));
    out.push(branch_finding(&repo));
    out
}

/// One size finding for one pool's root directory on this machine. Pools
/// whose slots live elsewhere (or whose root cannot be resolved) skip.
fn pool_root_finding(
    project: &Path,
    mailspace: &str,
    name: &str,
    config: &PoolConfig,
) -> Finding {
    let finding_name = format!("pool root {name}");
    if config.provider != "local" {
        return Finding::skipped(
            SECTION,
            &finding_name,
            &format!("slots of a `{}` pool do not live on this machine", config.provider),
        );
    }
    let Ok(site) =
        crate::pool::local_site::resolve(config.local.root_or_default(), project, mailspace, name)
    else {
        return Finding::skipped(SECTION, &finding_name, "pool root cannot be resolved");
    };
    size_finding(
        SECTION,
        &finding_name,
        &site.root,
        POOL_ROOT_WARN_GIB * BYTES_PER_GIB,
    )
}

/// One finding for one path's size: skipped when absent, warned above
/// `warn_bytes`, ok otherwise.
#[must_use]
pub fn size_finding(section: &str, name: &str, path: &Path, warn_bytes: u64) -> Finding {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Finding::skipped(section, name, &format!("{name} absent"));
    };
    let bytes = path_bytes(path, &meta);
    let gib = bytes / BYTES_PER_GIB;
    let tenths = (bytes % BYTES_PER_GIB) * 10 / BYTES_PER_GIB;
    if bytes > warn_bytes {
        Finding::warn(
            section,
            name,
            &format!(
                "{} is {gib}.{tenths} GiB (warns above {} GiB)",
                path.display(),
                warn_bytes / BYTES_PER_GIB
            ),
            RECLAIM_FIX,
        )
    } else {
        Finding::ok(
            section,
            name,
            &format!("{} is {gib}.{tenths} GiB", path.display()),
        )
    }
}

/// The bytes `path` holds, given its own metadata: a directory's recursive
/// walk, a file's length, anything else zero.
fn path_bytes(path: &Path, meta: &std::fs::Metadata) -> u64 {
    if meta.is_dir() {
        dir_size_bytes(path)
    } else if meta.is_file() {
        meta.len()
    } else {
        0
    }
}

/// The recursive size of `path` in bytes, file lengths only. Symlinks
/// contribute zero and are never followed; unreadable entries contribute
/// zero. Never errors.
fn dir_size_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_dir() {
        std::fs::read_dir(path)
            .map(|entries| entries.filter_map(Result::ok).map(dir_entry_size).sum())
            .unwrap_or(0)
    } else if meta.is_file() {
        meta.len()
    } else {
        0
    }
}

/// The size of one directory entry, through [`dir_size_bytes`].
fn dir_entry_size(entry: std::fs::DirEntry) -> u64 {
    dir_size_bytes(&entry.path())
}

/// The number of `unit/*` branches in the repository at `repo`, or `None`
/// when the clone is absent or git fails.
#[must_use]
pub fn branch_count(repo: &Path) -> Option<usize> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["for-each-ref", "--format=%(refname)", "refs/heads/unit/"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    Some(text.lines().filter(|line| !line.trim().is_empty()).count())
}

/// One finding for the live `unit/*` branch count in the integration clone.
fn branch_finding(repo: &Path) -> Finding {
    match branch_count(repo) {
        None => Finding::skipped(SECTION, "unit branches", "integration clone absent"),
        Some(count) if count > BRANCHES_WARN => Finding::warn(
            SECTION,
            "unit branches",
            &format!("{count} unit/* branches"),
            "vivi pump closeout prunes merged units",
        ),
        Some(count) => Finding::ok(SECTION, "unit branches", &format!("{count} unit/* branches")),
    }
}

#[cfg(test)]
#[path = "disk_section_test.rs"]
mod tests;
