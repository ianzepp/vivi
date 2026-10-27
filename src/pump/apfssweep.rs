//! Pump start: delete orphaned `vivi-*` APFS workspace volumes and the
//! worktrees they hold.
//!
//! A job's APFS volume is always deleted when its slot is unclaimed, so a
//! volume still present at pump start belongs to a process that crashed or
//! was killed, or survived a reboot. Unlike a RAM disk, an APFS volume
//! outlives a reboot: macOS mounts it again under `/Volumes`. This sweep runs
//! before the RAM sweep, and the RAM sweep skips any device this listing
//! names, so an APFS volume is never sent down the RAM eject path.
//!
//! A volume is Vivi's only when its name carries this mailspace's `vivi-`
//! prefix and it has a quota (see [`crate::pool::apfs::Volume::is_ours`]).
//!
//! This refines operator decision 7 (see [`crate::pump::ramsweep`]): a
//! volume is deleted only when its job has ended or its owner cannot be
//! found, not merely because it exists at pump start. A volume mounted over a
//! slot's target cache whose marker names it is kept while the slot's job
//! runs (live `boot.pid`) and deleted once it ended (`exit_code`, or a dead
//! `boot.pid`). A volume under a pool slot whose marker is missing or does
//! not match, or whose slot record is ambiguous, is kept with a logged
//! reason. A volume mounted outside any pool slot, or not mounted, has no
//! owner to ask and is deleted.

use std::path::{Path, PathBuf};

use crate::pool::apfs;
use crate::pool::ramdisk::{self, Disk, Runner};
use crate::pump::host::Ctx;
use crate::pump::ramsweep::{SlotState, remove_worktree, scope_prefix, slot_state};

/// Seconds the `mount` listing may take.
const LIST_SECS: u64 = 15;
/// One gibibyte, in bytes.
const GIB: u64 = 1024 * 1024 * 1024;

/// Delete this mailspace's orphaned APFS volumes. Returns one log line per
/// volume: `apfs sweep: deleted <volume>`, a kept-for-a-live-job line, or a
/// loud `WARNING`. Nothing is said when the machine lists no such volume or
/// cannot list APFS at all (not macOS).
#[must_use]
pub fn sweep_apfs(ctx: &Ctx, runner: &dyn Runner) -> Vec<String> {
    sweep_apfs_with(ctx, runner, slot_state)
}

/// The same sweep with the slot-state probe supplied by the caller.
pub(crate) fn sweep_apfs_with(
    ctx: &Ctx,
    runner: &dyn Runner,
    mut state_of: impl FnMut(&Path) -> SlotState,
) -> Vec<String> {
    let Ok(volumes) = apfs::ours(runner, &scope_prefix(ctx)) else {
        return Vec::new();
    };
    if volumes.is_empty() {
        return Vec::new();
    }
    let mounts = runner
        .run(&["mount"], LIST_SECS)
        .map(|text| ramdisk::parse_mount_rows(&text))
        .unwrap_or_default();
    let mut lines = Vec::new();
    for volume in volumes {
        let device = format!("/dev/{}", volume.device);
        let mount = mounts
            .iter()
            .find(|(listed, _)| ramdisk::same_device_id(listed, &device))
            .map(|(_, at)| PathBuf::from(at));
        let owner = mount.as_deref().and_then(|at| owner_slot(at, &volume.name));
        if owner.is_none()
            && mount
                .as_deref()
                .is_some_and(|at| ramdisk::pool_slot_for_mount(at).is_some())
        {
            lines.push(format!(
                "WARNING apfs sweep: preserving {}: its pool slot marker is missing or does not match",
                volume.name
            ));
            continue;
        }
        if let Some(slot) = &owner {
            match state_of(slot) {
                SlotState::Running => {
                    lines.push(format!(
                        "apfs sweep: preserving {} for live dispatched job",
                        volume.name
                    ));
                    continue;
                }
                SlotState::Unknown(why) => {
                    lines.push(format!(
                        "WARNING apfs sweep: preserving {}: {why}",
                        volume.name
                    ));
                    continue;
                }
                SlotState::Ended => {}
            }
        }
        if ctx.mailspace.is_empty() {
            lines.push(format!(
                "WARNING apfs sweep: no mailspace scope; deleting {}",
                volume.name
            ));
        }
        if let Some(at) = &mount {
            for dir in [at.join("repo"), at.join("w")] {
                remove_worktree(ctx, &dir);
            }
        }
        let disk = Disk {
            device,
            backing_device: None,
            volume: volume.name.clone(),
            size_gib: volume.quota_bytes / GIB,
            mount: mount.unwrap_or_default(),
        };
        lines.push(delete_line(runner, &disk, owner.as_deref()));
    }
    lines
}

/// The slot whose APFS marker names exactly this volume at this mount.
fn owner_slot(mount: &Path, volume: &str) -> Option<PathBuf> {
    let slot = ramdisk::pool_slot_for_mount(mount)?;
    let Ok(marker) = std::fs::read_to_string(slot.join(apfs::SLOT_MARKER)) else {
        return None;
    };
    let has = |line: String| marker.lines().any(|l| l == line);
    (has(format!("volume={volume}")) && has(format!("mount={}", mount.display()))).then_some(slot)
}

/// Delete one volume and, when a slot marker recorded it, drop the marker.
fn delete_line(runner: &dyn Runner, disk: &Disk, owner: Option<&Path>) -> String {
    match apfs::delete(runner, disk) {
        Ok(()) => {
            if let Some(slot) = owner {
                drop(std::fs::remove_file(slot.join(apfs::SLOT_MARKER)));
            }
            format!("apfs sweep: deleted {}", disk.volume)
        }
        Err(error) => format!("WARNING apfs sweep: {} delete failed: {error}", disk.volume),
    }
}

#[cfg(test)]
#[path = "apfssweep_test.rs"]
mod tests;
