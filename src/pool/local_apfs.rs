//! The local provider's per-job APFS volume: `kind = "apfs"` in
//! `[pools.<name>.workspace]`.
//!
//! The same contract as the per-job RAM disk in the parent module: admitted
//! before the job, mounted over the job's `<slot>/targets/<key>` cache,
//! recorded in a marker on the slot root, and deleted when the slot is
//! unclaimed or destroyed or the next job starts. The marker is the file
//! `apfs`, never `ramdisk`, so the RAM recovery and sweep paths never see an
//! APFS volume and never send one down their eject path.
//!
//! Admission checks two things under the host-wide admission lock: free
//! memory for the compilers (the margin), and free space in the container
//! for the quota.

use std::path::{Path, PathBuf};

use super::slots;
use super::{LocalProvider, available_memory, pool_job_volume, remove_mount_worktree};
use crate::error::VivariumError;
use crate::pool::apfs;
use crate::pool::ramdisk::{self, Disk, Runner};

/// The slot-root file that records a job's APFS volume.
pub(super) const MARKER: &str = apfs::SLOT_MARKER;

/// Admit, make and mount the job's volume at `mount`, then record it on the
/// slot root. A volume whose marker cannot be written is deleted again.
pub(super) fn mount_at(
    provider: &LocalProvider,
    id: &str,
    job: &str,
    slot: &Path,
    mount: &Path,
    source: &str,
) -> Result<(), VivariumError> {
    let runner = provider.ramdisk_runner.as_ref();
    let _admission_lock = ramdisk::lock_admission().map_err(VivariumError::Message)?;
    let want = provider.workspace.size_gib;
    let margin = provider.workspace.margin_gib;
    ramdisk::admit_with(runner, available_memory(provider), 0, margin)
        .map_err(VivariumError::Message)?;
    let container = match provider.workspace.container.clone() {
        Some(container) => container,
        None => apfs::container_of(runner, slot).map_err(VivariumError::Message)?,
    };
    apfs::admit(runner, &container, want, margin).map_err(VivariumError::Message)?;
    let volume = pool_job_volume(id, job)?;
    let disk =
        apfs::create(runner, &container, &volume, want, mount).map_err(VivariumError::Message)?;
    let marker = marker_text(&disk, job, source, &container);
    if let Err(error) = slots::replace_file(&slot.join(MARKER), marker.as_bytes()) {
        if let Err(delete) = apfs::delete(runner, &disk) {
            eprintln!(
                "warning: APFS volume {} delete failed after marker write error: {delete}",
                disk.volume
            );
        }
        return Err(error);
    }
    Ok(())
}

fn marker_text(disk: &Disk, job: &str, source: &str, container: &str) -> String {
    format!(
        "volume_device={}\nvolume={}\nsize_gib={}\njob={job}\nmount={}\nsource={source}\ncontainer={container}\n",
        disk.device,
        disk.volume,
        disk.size_gib,
        disk.mount.display()
    )
}

/// The fields of a slot's APFS marker, confined to its own
/// `<slot>/targets/<key>` mount and to the volume name its job implies.
struct Record {
    disk: Disk,
    source: Option<PathBuf>,
}

fn record(marker: &str, dir: &Path, id: &str) -> Option<Record> {
    let field = |name: &str| {
        marker
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
    };
    let device = ramdisk::normalized_disk_id(field("volume_device")?)?;
    let volume = field("volume")?.to_string();
    let job = field("job")?;
    let Ok(size_gib) = field("size_gib")?.parse() else {
        return None;
    };
    let mount = PathBuf::from(field("mount")?);
    let Ok(relative) = mount.strip_prefix(dir.join("targets")) else {
        return None;
    };
    let one_target = relative.components().count() == 1
        && relative
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)));
    let implied = pool_job_volume(id, job).is_ok_and(|implied| implied == volume);
    if job.is_empty() || !one_target || !implied {
        return None;
    }
    let source = field("source")
        .filter(|source| !source.is_empty())
        .map(PathBuf::from);
    Some(Record {
        disk: Disk {
            device: format!("/dev/{device}"),
            backing_device: None,
            volume,
            size_gib,
            mount,
        },
        source,
    })
}

/// Delete the APFS volume the slot's marker records, if any: detach its
/// worktree, delete the volume after proving it is ours, then drop the
/// marker. A malformed marker or a failed step keeps the marker for retry and
/// diagnosis.
pub(super) fn teardown(dir: &Path, id: &str, runner: &dyn Runner) -> Result<(), VivariumError> {
    let marker_path = dir.join(MARKER);
    let text = match marker_path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(metadata) if metadata.file_type().is_file() => std::fs::read_to_string(&marker_path)?,
        _ => return Err(malformed(&marker_path)),
    };
    let Some(record) = record(&text, dir, id) else {
        return Err(malformed(&marker_path));
    };
    if let Some(error) = remove_mount_worktree(record.source.as_deref(), &record.disk.mount) {
        return Err(VivariumError::Message(format!(
            "APFS volume {} cleanup incomplete: {error}",
            record.disk.volume
        )));
    }
    apfs::delete(runner, &record.disk).map_err(|error| {
        eprintln!(
            "warning: APFS volume {} delete failed: {error}",
            record.disk.volume
        );
        VivariumError::Message(error)
    })?;
    std::fs::remove_file(&marker_path).map_err(|error| {
        VivariumError::Message(format!(
            "APFS volume {} deleted but marker removal failed: {error}",
            record.disk.volume
        ))
    })?;
    eprintln!(
        "APFS volume {} deleted ({} GiB quota released)",
        record.disk.volume, record.disk.size_gib
    );
    Ok(())
}

fn malformed(path: &Path) -> VivariumError {
    let detail = format!(
        "malformed APFS volume marker {}; preserving it",
        path.display()
    );
    eprintln!("warning: {detail}");
    VivariumError::Message(detail)
}

#[cfg(test)]
#[path = "local_apfs_test.rs"]
mod tests;
