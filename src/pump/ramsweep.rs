//! Pump start: sweep orphaned `vivi-*` RAM disks and the worktrees they hold.
//!
//! Operator decision 7 (2026-10-19): a pump start sweeps orphaned
//! `vivi-*` RAM disks. A job's disk is always torn down when the job ends, so
//! a disk still attached at start belongs to a process that crashed, was
//! killed, or rebooted away. Its worktree — `<mount>/repo` or `<mount>/w` —
//! is registered with a repository outside the disk, and that registration
//! survives the crash: removing the disk alone would leave a worktree entry
//! pointing at a path that no longer exists.
//!
//! The sweep therefore detaches each matching worktree through its owning
//! repository (`git -C <owner> worktree remove --force <dir>`, never a
//! repository-wide `git worktree prune`) and then tears the disk down. This
//! is crash and reboot recovery, distinct from [`crate::pool::clean_work`],
//! which sweeps worktrees whose unit is done while the pump is running.

use std::path::{Path, PathBuf};

use crate::pool::ramdisk::{self, Disk, Runner};
use crate::pump::host::{Ctx, shell_quote};

/// Seconds one `git` call in the sweep may take.
const GIT_SECS: u64 = 120;

/// Sweep the orphaned RAM disks this mailspace owns: detach every worktree
/// they still register, then tear each disk down. Returns one log line per
/// disk for the caller to write: `ramdisk sweep: detached <volume>` on
/// success, and a loud `WARNING` on a failed teardown.
///
/// Volume scoping is `vivi-<mailspace>-`; when the context carries no
/// mailspace id the owner cannot be identified, so every `vivi-` disk is
/// swept and each line says the scope is off.
#[must_use]
pub fn sweep_ramdisks(ctx: &Ctx, runner: &dyn crate::pool::ramdisk::Runner) -> Vec<String> {
    let scope = scope_prefix(ctx);
    let mut lines = Vec::new();
    for disk in ramdisk::mounted(runner) {
        if !disk.volume.starts_with(&scope) {
            continue;
        }
        if ctx.mailspace.is_empty() {
            lines.push(format!(
                "WARNING ramdisk sweep: no mailspace scope; detaching {}",
                disk.volume
            ));
        }
        for dir in [disk.mount.join("repo"), disk.mount.join("w")] {
            remove_worktree(ctx, &dir);
        }
        lines.push(teardown_line(runner, &disk));
    }
    lines
}

/// The volume prefix this sweep claims: `vivi-<mailspace>-`, or the bare
/// `vivi-` prefix when the context names no mailspace.
fn scope_prefix(ctx: &Ctx) -> String {
    if ctx.mailspace.is_empty() {
        ramdisk::VOLUME_PREFIX.to_string()
    } else {
        format!("{}{}-", ramdisk::VOLUME_PREFIX, ctx.mailspace)
    }
}

/// Detach `dir` through the repository that owns it, when `dir` is a
/// worktree at all. A missing `.git`, an unreadable owner, or a refused
/// removal leaves the caller to tear the disk down anyway.
fn remove_worktree(ctx: &Ctx, dir: &Path) {
    if !dir.join(".git").exists() {
        return;
    }
    let Some(owner) = owner_repo(ctx, dir) else {
        return;
    };
    let owner = shell_quote(&owner);
    let dir = shell_quote(&dir.to_string_lossy());
    drop(ctx.sh(
        &format!("git -C {owner} worktree remove --force {dir}"),
        None,
        GIT_SECS,
    ));
}

/// The repository root that owns the worktree at `dir`: `git rev-parse
/// --git-common-dir` names the shared `.git` directory, whose parent is the
/// checkout the worktree is registered with.
fn owner_repo(ctx: &Ctx, dir: &Path) -> Option<String> {
    let quoted = shell_quote(&dir.to_string_lossy());
    let out = ctx.sh(
        &format!("git -C {quoted} rev-parse --git-common-dir"),
        None,
        GIT_SECS,
    );
    if out.code != 0 {
        return None;
    }
    let raw = out.text.lines().next()?.trim();
    if raw.is_empty() {
        return None;
    }
    let common = Path::new(raw);
    let absolute = if common.is_absolute() {
        common.to_path_buf()
    } else {
        dir.join(common)
    };
    Some(owner_of(&absolute))
}

/// The checkout holding `common`: its parent when it is a `.git` directory,
/// otherwise `common` itself (a bare repository or a separate git dir).
fn owner_of(common: &Path) -> String {
    let dot_git = common.file_name().is_some_and(|name| name == ".git");
    let owner: PathBuf = if dot_git {
        common
            .parent()
            .map_or_else(|| common.to_path_buf(), Path::to_path_buf)
    } else {
        common.to_path_buf()
    };
    owner.to_string_lossy().into_owned()
}

/// The single log line for one disk: detached, or a loud warning naming what
/// survived.
fn teardown_line(runner: &dyn Runner, disk: &Disk) -> String {
    match ramdisk::teardown(runner, disk) {
        Ok(()) => format!("ramdisk sweep: detached {}", disk.volume),
        Err(error) => format!(
            "WARNING ramdisk sweep: {} teardown failed: {error}",
            disk.volume
        ),
    }
}
