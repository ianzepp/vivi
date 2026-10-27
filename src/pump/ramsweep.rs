//! Pump start: sweep orphaned `vivi-*` RAM disks and the worktrees they hold.
//!
//! Operator decision 7 (2026-10-19): a pump start sweeps orphaned
//! `vivi-*` RAM disks. A job's disk is always torn down when the job ends, so
//! a disk still attached at start belongs to a process that crashed, was
//! killed, or rebooted away.
//!
//! **Refinement of decision 7.** That premise holds after a crash but not
//! after a deliberate restart: restarting the server restarts every pump
//! while jobs on local-pool slots keep running detached. So a disk is removed
//! only when its job has ended or its owner cannot be found, never because it
//! is merely attached. The signal is the slot's own record
//! ([`slot_state`]): a live `boot.pid` (matching `boot.start`) means the job
//! runs and the disk is kept; an `exit_code` file, or a dead `boot.pid`,
//! means the job ended and the disk goes. A disk that names no slot (not a
//! pool mount) has no owner to ask and goes. Anything ambiguous — a pool
//! mount whose marker does not match, a slot with no boot record, a process
//! table that cannot be read — keeps the disk and says why; a stale disk
//! costs space, a removed live one destroys work. Each disk gets one log
//! line, kept or removed. Its worktree — `<mount>/repo` or `<mount>/w` —
//! is registered with a repository outside the disk, and that registration
//! survives the crash: removing the disk alone would leave a worktree entry
//! pointing at a path that no longer exists.
//!
//! The sweep therefore detaches each matching worktree through its owning
//! repository (`git -C <owner> worktree remove --force <dir>`, never a
//! repository-wide `git worktree prune`) and then tears the disk down. This
//! is crash and reboot recovery.

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
    sweep_ramdisks_with(ctx, runner, slot_state)
}

/// What a pool slot's own record says about its job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SlotState {
    /// The boot wrapper is alive: the job runs.
    Running,
    /// The job ended or its process is gone: the disk is an orphan.
    Ended,
    /// The record does not settle it; keep the disk. Carries the reason.
    Unknown(&'static str),
}

/// Decide a slot's state from its facts: whether the boot wrapper is alive,
/// whether `exit_code` and `boot.pid` exist, and whether the process table
/// could be read. Pure, so the keep/remove rule is testable without a slot.
pub(crate) fn classify_slot(
    boot_alive: bool,
    has_exit_code: bool,
    has_boot_pid: bool,
    ps_readable: bool,
) -> SlotState {
    if boot_alive {
        SlotState::Running
    } else if has_exit_code {
        SlotState::Ended
    } else if has_boot_pid {
        if ps_readable {
            SlotState::Ended
        } else {
            SlotState::Unknown("cannot read the process table")
        }
    } else {
        SlotState::Unknown("its slot has no boot record")
    }
}

/// The state of the job in `slot`, from the files and processes the local
/// pool already keeps (`boot.pid`, `boot.start`, `exit_code`).
pub(crate) fn slot_state(slot: &Path) -> SlotState {
    use crate::pool::local_proc as proc;
    let alive = proc::is_alive(slot);
    let has_exit_code = slot.join(proc::EXIT_CODE).exists();
    let has_boot_pid = slot.join(proc::BOOT_PID).exists();
    let ps_readable = alive || has_exit_code || !has_boot_pid || proc::listing().is_ok();
    classify_slot(alive, has_exit_code, has_boot_pid, ps_readable)
}

/// The same sweep with the slot-state probe supplied by the caller. Kept
/// private so tests can prove that running jobs keep their mounts without
/// depending on a real process table.
pub(crate) fn sweep_ramdisks_with(
    ctx: &Ctx,
    runner: &dyn crate::pool::ramdisk::Runner,
    mut state_of: impl FnMut(&Path) -> SlotState,
) -> Vec<String> {
    let scope = scope_prefix(ctx);
    let mut lines = Vec::new();
    let disks: Vec<Disk> = ramdisk::mounted(runner)
        .into_iter()
        .filter(|disk| disk.volume.starts_with(&scope))
        .collect();
    if disks.is_empty() {
        return lines;
    }
    let apfs_devices = match apfs_workspace_devices(runner) {
        Ok(devices) => devices,
        Err(error) => {
            lines.push(format!(
                "WARNING ramdisk sweep: cannot list APFS volumes ({error}); skipping so no APFS workspace volume is ejected"
            ));
            return lines;
        }
    };
    for disk in disks {
        if apfs_devices
            .iter()
            .any(|device| ramdisk::same_device_id(device, &disk.device))
        {
            lines.push(format!(
                "ramdisk sweep: skipping {}: an APFS workspace volume, not a RAM disk",
                disk.volume
            ));
            continue;
        }
        let pool_mount = ramdisk::pool_slot_for_mount(&disk.mount).is_some();
        let pool_owner = pool_owner_for_disk(&disk, runner);
        if pool_mount && pool_owner.is_none() {
            lines.push(format!(
                "WARNING ramdisk sweep: preserving {} because its pool ownership marker does not match the attached device",
                disk.volume
            ));
            continue;
        }
        if let Some(owner) = &pool_owner {
            match state_of(&owner.slot) {
                SlotState::Running => {
                    lines.push(format!(
                        "ramdisk sweep: preserving {} for live dispatched job",
                        disk.volume
                    ));
                    continue;
                }
                SlotState::Unknown(why) => {
                    lines.push(format!(
                        "WARNING ramdisk sweep: preserving {}: {why}",
                        disk.volume
                    ));
                    continue;
                }
                SlotState::Ended => {}
            }
        }
        if ctx.mailspace.is_empty() {
            lines.push(format!(
                "WARNING ramdisk sweep: no mailspace scope; detaching {}",
                disk.volume
            ));
        }
        if let Some(owner) = pool_owner {
            if !owner.source.is_empty() {
                let worktree = disk.mount.join("repo");
                remove_worktree(ctx, &worktree);
            }
        } else {
            for dir in [disk.mount.join("repo"), disk.mount.join("w")] {
                remove_worktree(ctx, &dir);
            }
        }
        lines.push(teardown_line(runner, &disk));
    }
    lines
}

/// The devices of every Vivi APFS workspace volume. A RAM teardown ejects the
/// whole backing disk, which for an APFS volume would be the machine's own
/// disk, so the RAM sweep never touches a device this names.
fn apfs_workspace_devices(runner: &dyn Runner) -> Result<Vec<String>, String> {
    let (_, volumes) = crate::pool::apfs::list(runner)?;
    Ok(volumes
        .into_iter()
        .filter(crate::pool::apfs::Volume::is_ours)
        .map(|volume| volume.device)
        .collect())
}

/// The local slot holding `disk`, but only when its marker still names this
/// exact mount and volume. Pool jobs run in detached slot processes, so a
/// matching live slot owns the disk across pump restarts.
struct PoolRamOwner {
    slot: PathBuf,
    source: String,
}

/// The slot and local source recorded for a pool RAM disk, but only when the
/// marker still names the exact mounted volume and path.
fn pool_owner_for_disk(
    disk: &Disk,
    runner: &dyn crate::pool::ramdisk::Runner,
) -> Option<PoolRamOwner> {
    let slot = ramdisk::pool_slot_for_mount(&disk.mount)?;
    let Ok(marker) = std::fs::read_to_string(slot.join("ramdisk")) else {
        return None;
    };
    let field = |name: &str| {
        marker
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
    };
    let marked_device = field("device")?;
    let marked_volume_device = field("volume_device")?;
    if field("volume") != Some(disk.volume.as_str())
        || !ramdisk::same_device_id(marked_volume_device, &disk.device)
        || field("mount") != Some(disk.mount.to_string_lossy().as_ref())
        || !field("job").is_some_and(|job| !job.is_empty())
    {
        return None;
    }
    let actual_backing =
        match ramdisk::backing_device_for_volume(runner, &disk.device, &disk.volume) {
            Ok(backing) => backing,
            Err(error) => {
                eprintln!(
                    "warning: RAM disk {} backing verification failed: {error}",
                    disk.volume
                );
                return None;
            }
        };
    if !ramdisk::same_device_id(marked_device, &actual_backing) {
        return None;
    }
    Some(PoolRamOwner {
        slot,
        source: field("source").unwrap_or_default().to_string(),
    })
}

/// The volume prefix this sweep claims: `vivi-<mailspace>-`, or the bare
/// `vivi-` prefix when the context names no mailspace.
pub(crate) fn scope_prefix(ctx: &Ctx) -> String {
    if ctx.mailspace.is_empty() {
        ramdisk::VOLUME_PREFIX.to_string()
    } else {
        format!("{}{}-", ramdisk::VOLUME_PREFIX, ctx.mailspace)
    }
}

/// Detach `dir` through the repository that owns it, when `dir` is a
/// worktree at all. A missing `.git`, an unreadable owner, or a refused
/// removal leaves the caller to tear the disk down anyway.
pub(crate) fn remove_worktree(ctx: &Ctx, dir: &Path) {
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
