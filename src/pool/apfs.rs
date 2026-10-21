//! APFS quota volumes as job workspace disks: the `apfs` kind of
//! `[pools.<name>.workspace]`.
//!
//! A job's volume is made fresh in an APFS container on this machine, mounted
//! where the job works, and deleted when the job ends, the same lifecycle as
//! a RAM disk (see [`super::ramdisk`]). The difference is what backs it: the
//! volume shares the container's free space and is held to its size by a
//! quota, so a job that overruns gets ENOSPC inside its own volume while the
//! rest of the machine is untouched.
//!
//! Rules this module keeps:
//!
//! - **Quota only, never a reserve.** A reserve claims its space up front and
//!   cannot be shrunk in place; the create command never carries `-reserve`.
//! - **Created unmounted, then mounted with `noowners`.** `addVolume
//!   -mountpoint` needs root, while `diskutil mount -mountPoint` does not; the
//!   `noowners` option makes the volume writable by the user who runs `vivi`.
//! - **Delete only what is provably ours.** A volume is deleted only when the
//!   live listing shows the recorded device still holds a `vivi-*` volume of
//!   the recorded name with a quota. Ejecting is never used: these volumes
//!   live in a container on a real disk.
//!
//! Every `diskutil` call goes through [`Runner`], so tests replace it with
//! canned output.

use std::path::Path;

use serde_json::Value;

use super::ramdisk::{Disk, Runner, VOLUME_PREFIX, normalized_disk_id, plist_json, plist_string};

/// Seconds one `diskutil` call may take.
const DISKUTIL_SECS: u64 = 60;
/// One gibibyte, in bytes.
const GIB: u64 = 1024 * 1024 * 1024;
/// The slot-root file in which the local provider records a job.s APFS volume;
/// the pump-start sweep reads it to find the owning slot.
pub const SLOT_MARKER: &str = "apfs";
/// The line `diskutil apfs addVolume` prints with the new volume's device.
const CREATED: &str = "Disk from APFS operation:";

/// One APFS volume as `diskutil apfs list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// Device identifier, `diskNsM`.
    pub device: String,
    pub name: String,
    /// The container's device identifier, `diskN`.
    pub container: String,
    pub quota_bytes: u64,
    pub in_use_bytes: u64,
}

impl Volume {
    /// A volume Vivi made: named `vivi-*` and held by a quota.
    #[must_use]
    pub fn is_ours(&self) -> bool {
        self.name.starts_with(VOLUME_PREFIX) && self.quota_bytes > 0
    }
}

/// One APFS container and its unallocated space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    /// Device identifier, `diskN`.
    pub reference: String,
    pub free_bytes: u64,
}

/// Containers and volumes from the JSON form of `diskutil apfs list -plist`.
/// Rows with no valid device identifier are dropped.
#[must_use]
pub fn parse_list(value: &Value) -> (Vec<Container>, Vec<Volume>) {
    let mut containers = Vec::new();
    let mut volumes = Vec::new();
    let empty = Vec::new();
    for entry in value
        .get("Containers")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
    {
        let Some(reference) =
            plist_string(entry, "ContainerReference").and_then(normalized_disk_id)
        else {
            continue;
        };
        containers.push(Container {
            reference: reference.clone(),
            free_bytes: number(entry, "CapacityFree"),
        });
        for volume in entry
            .get("Volumes")
            .and_then(Value::as_array)
            .unwrap_or(&empty)
        {
            let Some(device) =
                plist_string(volume, "DeviceIdentifier").and_then(normalized_disk_id)
            else {
                continue;
            };
            volumes.push(Volume {
                device,
                name: plist_string(volume, "Name").unwrap_or_default().to_string(),
                container: reference.clone(),
                quota_bytes: number(volume, "CapacityQuota"),
                in_use_bytes: number(volume, "CapacityInUse"),
            });
        }
    }
    (containers, volumes)
}

fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or_default()
}

/// Every APFS container and volume on this machine.
///
/// # Errors
/// Returns an error when `diskutil` or the plist conversion fails.
pub fn list(runner: &dyn Runner) -> Result<(Vec<Container>, Vec<Volume>), String> {
    let xml = runner.run(&["diskutil", "apfs", "list", "-plist"], DISKUTIL_SECS)?;
    Ok(parse_list(&plist_json(runner, &xml, "APFS list")?))
}

/// The APFS container that holds `path`.
///
/// # Errors
/// Returns an error when `path` is not on an APFS volume or `diskutil` fails.
pub fn container_of(runner: &dyn Runner, path: &Path) -> Result<String, String> {
    let at = path.to_string_lossy();
    let xml = runner.run(&["diskutil", "info", "-plist", &at], DISKUTIL_SECS)?;
    let info = plist_json(runner, &xml, "path disk info")?;
    plist_string(&info, "APFSContainerReference")
        .and_then(normalized_disk_id)
        .ok_or_else(|| format!("apfs: {at} is not on an APFS volume"))
}

/// Whether a `want_gib` quota fits: the container's free space, less what the
/// existing Vivi volumes may still grow into and the margin, covers it.
#[must_use]
pub fn admits(free_bytes: u64, outstanding_bytes: u64, want_gib: u64, margin_gib: u64) -> bool {
    let need = want_gib.saturating_add(margin_gib).saturating_mul(GIB);
    free_bytes.saturating_sub(outstanding_bytes) >= need
}

/// Admit a `want_gib` volume into `container`, or say why it must wait.
///
/// # Errors
/// Returns a wait message when the space does not fit, or the listing error.
pub fn admit(
    runner: &dyn Runner,
    container: &str,
    want_gib: u64,
    margin_gib: u64,
) -> Result<(), String> {
    let (containers, volumes) = list(runner)?;
    let free = containers
        .iter()
        .find(|c| c.reference == container)
        .map(|c| c.free_bytes)
        .ok_or_else(|| format!("apfs: container {container} is not in the APFS list"))?;
    let outstanding: u64 = volumes
        .iter()
        .filter(|v| v.container == container && v.is_ours())
        .map(|v| v.quota_bytes.saturating_sub(v.in_use_bytes))
        .sum();
    if admits(free, outstanding, want_gib, margin_gib) {
        return Ok(());
    }
    Err(format!(
        "apfs-wait: a {want_gib} GiB volume does not fit in {container}: {} GiB free, {} GiB promised to other Vivi volumes, {margin_gib} GiB margin",
        free / GIB,
        outstanding / GIB
    ))
}

/// The command that makes volume `name` in `container` with a `gib` quota,
/// unmounted. Never a reserve.
#[must_use]
pub fn create_command(container: &str, name: &str, gib: u64) -> Vec<String> {
    [
        "diskutil",
        "apfs",
        "addVolume",
        container,
        "APFS",
        name,
        "-quota",
    ]
    .iter()
    .map(ToString::to_string)
    .chain([format!("{gib}g"), "-nomount".to_string()])
    .collect()
}

/// The device `diskutil apfs addVolume` reports for the volume it made.
#[must_use]
pub fn created_device(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix(CREATED))
        .and_then(|rest| normalized_disk_id(rest.trim()))
}

/// Make volume `name` in `container` with a `gib` quota and mount it at `at`.
/// A volume that cannot be mounted is deleted again, so a half-made volume
/// never stays.
///
/// # Errors
/// Returns an error when the name is not Vivi's, the mount directory cannot
/// be made, or a `diskutil` step refuses.
pub fn create(
    runner: &dyn Runner,
    container: &str,
    name: &str,
    gib: u64,
    at: &Path,
) -> Result<Disk, String> {
    if !name.starts_with(VOLUME_PREFIX) {
        return Err(format!("apfs: refusing to make non-Vivi volume {name}"));
    }
    std::fs::create_dir_all(at)
        .map_err(|e| format!("apfs: cannot create {}: {e}", at.display()))?;
    let argv = create_command(container, name, gib);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let made = runner.run(&argv, DISKUTIL_SECS)?;
    let device = created_device(&made)
        .ok_or_else(|| format!("apfs: addVolume named no device for {name}: {made}"))?;
    let mount_at = at.to_string_lossy();
    let mounted = runner.run(
        &[
            "diskutil",
            "mount",
            "-mountOptions",
            "noowners",
            "-mountPoint",
            &mount_at,
            &device,
        ],
        DISKUTIL_SECS,
    );
    if let Err(error) = mounted {
        drop(runner.run(
            &["diskutil", "apfs", "deleteVolume", &device],
            DISKUTIL_SECS,
        ));
        return Err(format!("apfs: cannot mount {name} at {mount_at}: {error}"));
    }
    Ok(Disk {
        device: format!("/dev/{device}"),
        backing_device: None,
        volume: name.to_string(),
        size_gib: gib,
        mount: at.to_path_buf(),
    })
}

/// Delete the volume `disk` names, after proving from the live listing that
/// its device still holds that `vivi-*` quota volume, then verify it is gone.
/// A volume already gone is success.
///
/// # Errors
/// Returns an error when the device now holds another volume, or when the
/// volume survives the delete.
pub fn delete(runner: &dyn Runner, disk: &Disk) -> Result<(), String> {
    let device = normalized_disk_id(&disk.device)
        .ok_or_else(|| format!("apfs: invalid device {}", disk.device))?;
    let (_, volumes) = list(runner)?;
    let Some(volume) = volumes.iter().find(|v| v.device == device) else {
        return Ok(());
    };
    if volume.name != disk.volume || !volume.is_ours() {
        return Err(format!(
            "apfs: {device} holds {} now, not Vivi volume {}; refusing to delete",
            volume.name, disk.volume
        ));
    }
    delete_device(runner, &device)?;
    let (_, volumes) = list(runner)?;
    if volumes
        .iter()
        .any(|v| v.device == device && v.name == disk.volume)
    {
        return Err(format!("{} still present after delete", disk.volume));
    }
    Ok(())
}

/// `deleteVolume`, forcing the unmount once when a busy volume refuses.
fn delete_device(runner: &dyn Runner, device: &str) -> Result<(), String> {
    if runner
        .run(&["diskutil", "apfs", "deleteVolume", device], DISKUTIL_SECS)
        .is_ok()
    {
        return Ok(());
    }
    drop(runner.run(&["diskutil", "unmount", "force", device], DISKUTIL_SECS));
    runner
        .run(&["diskutil", "apfs", "deleteVolume", device], DISKUTIL_SECS)
        .map(drop)
}

/// The Vivi volumes whose names start with `prefix`, for an orphan sweep.
///
/// # Errors
/// Returns the listing error.
pub fn ours(runner: &dyn Runner, prefix: &str) -> Result<Vec<Volume>, String> {
    let (_, volumes) = list(runner)?;
    Ok(volumes
        .into_iter()
        .filter(|v| v.is_ours() && v.name.starts_with(prefix))
        .collect())
}

#[cfg(test)]
#[path = "apfs_test.rs"]
mod tests;
