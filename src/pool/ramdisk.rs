//! Per-job RAM disks for local macOS jobs: the pure naming, sizing,
//! admission, and parser helpers shared by the mount and teardown paths.
//!
//! Operator design (2026-10-19): every job gets a fresh RAM disk, created at
//! dispatch and always torn down when the job ends — success, failure,
//! timeout, or cancel. There are no standing or shared disks. The disk holds
//! the job's worktree and build directory (`target`), and is named
//! `vivi-<mailspace>-<slot>-<job>` so orphans stay recognizable. Before a job
//! starts, admission checks that free memory (minus memory already held by
//! other disks and a compiler safety margin) covers the job size; when it
//! does not, the job stays queued and the caller reports [`wait_error`].
//!
//! Two layers live here. The pure core — naming, sizing, admission, and the
//! `mount` / `df -g` parsers — has no process spawning. The OS layer shells
//! out to `hdiutil` and `diskutil` through [`Runner`], which is the seam
//! tests replace with canned output: [`mount`] creates a disk, [`teardown`]
//! ejects it and verifies the memory came back, and [`mounted`] reports the
//! disks that are attached now. RAM disks exist only on macOS; elsewhere
//! [`RealRunner`] refuses, since a Linux tmpfs disk is a separate follow-up.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

use sysinfo::{MemoryRefreshKind, RefreshKind, System};

/// Every RAM disk volume name starts with this, so orphan sweeps and
/// parsers can recognize our disks.
pub const VOLUME_PREFIX: &str = "vivi-";

/// The refusal of every [`RealRunner`] call away from macOS.
const NOT_MACOS: &str = "ram disks need macOS (hdiutil); Linux tmpfs is a follow-up";

/// Seconds `hdiutil attach` may take.
const ATTACH_SECS: u64 = 120;
/// Seconds one `diskutil` call may take.
const DISKUTIL_SECS: u64 = 60;
/// Seconds a listing or a post-teardown verification may take.
const LIST_SECS: u64 = 15;
/// How long [`RealRunner`] sleeps between liveness checks.
const POLL: Duration = Duration::from_millis(20);

/// One gibibyte, in bytes.
const GIB: u64 = 1024 * 1024 * 1024;

/// The RAM disk volume name for `parts` (mailspace, slot, job): joined with
/// `-`, every `/` or whitespace replaced by `-`, prefixed `vivi-`.
#[must_use]
pub fn volume_name(parts: &[&str]) -> String {
    let sanitized: String = parts
        .join("-")
        .chars()
        .map(|c| {
            if c == '/' || c.is_whitespace() {
                '-'
            } else {
                c
            }
        })
        .collect();
    format!("{VOLUME_PREFIX}{sanitized}")
}

/// 512-byte sectors on a `gib` GiB disk — the unit `hdiutil attach ram://`
/// expects.
#[must_use]
pub fn sectors(gib: u64) -> u64 {
    gib * GIB / 512
}

/// Whether `available` bytes of free memory admit a job wanting `want_gib`
/// when `held_gib` is already committed to other RAM disks and `margin_gib`
/// must stay free for the compiler and the system.
#[must_use]
pub fn admits(available: u64, held_gib: u64, want_gib: u64, margin_gib: u64) -> bool {
    let needed_bytes = held_gib
        .saturating_add(margin_gib)
        .saturating_add(want_gib)
        .saturating_mul(GIB);
    available >= needed_bytes
}

/// The queued-job message when admission fails. The `ramdisk-wait:` prefix
/// is a cross-unit contract; other units match on it.
#[must_use]
pub fn wait_error(want_gib: u64, free_gib: u64, held_gib: u64, margin_gib: u64) -> String {
    format!(
        "ramdisk-wait: need {want_gib} GiB, {free_gib} GiB free, \
         {held_gib} GiB held, {margin_gib} GiB margin"
    )
}

/// The memory held by the RAM disks attached now: the sum of the sizes
/// [`mounted`] reports. A disk missing from the `df` listing counts as zero.
#[must_use]
pub fn held_gib(runner: &dyn Runner) -> u64 {
    mounted(runner).iter().map(|disk| disk.size_gib).sum()
}

/// The memory held by attached RAM disks, or an error when either inventory
/// listing failed and the admission total cannot be trusted.
pub fn held_gib_checked(runner: &dyn Runner) -> Result<u64, String> {
    Ok(mounted_checked(runner)?
        .iter()
        .map(|disk| disk.size_gib)
        .sum())
}

/// Admit a job wanting `want_gib` against `available` bytes of memory, with
/// `margin_gib` kept free for the compiler and the system. This is the half
/// tests drive directly, since it never reads real memory.
///
/// # Errors
/// Returns [`wait_error`], whose `ramdisk-wait:` prefix is the cross-unit
/// contract, when the job does not fit. The job then waits queued: there is no
/// SSD fallback.
pub fn admit_with(
    runner: &dyn Runner,
    available: u64,
    want_gib: u64,
    margin_gib: u64,
) -> Result<(), String> {
    let held = held_gib_checked(runner)
        .map_err(|error| format!("ramdisk-wait: cannot inventory held disks: {error}"))?;
    if admits(available, held, want_gib, margin_gib) {
        return Ok(());
    }
    Err(wait_error(want_gib, available / GIB, held, margin_gib))
}

/// Admit a job wanting `want_gib` against the memory the OS reports free now.
///
/// # Errors
/// Same as [`admit_with`]: a `ramdisk-wait:` error means the job waits queued.
pub fn admit(runner: &dyn Runner, want_gib: u64, margin_gib: u64) -> Result<(), String> {
    admit_with(runner, available_bytes(), want_gib, margin_gib)
}

/// Available memory in bytes, as `sysinfo` reports it right now.
pub(crate) fn available_bytes() -> u64 {
    let system = System::new_with_specifics(
        RefreshKind::new().with_memory(MemoryRefreshKind::new().with_ram()),
    );
    system.available_memory()
}

/// Whether `text` reports a full disk (ENOSPC): the kernel phrase or the
/// `os error 28` form.
#[must_use]
pub fn enospc(text: &str) -> bool {
    text.contains("No space left on device") || text.contains("os error 28")
}

/// `(device, mount point)` pairs from `mount` output lines shaped
/// `<dev> on <mount> (<options>)`, keeping only mounts whose final path
/// component starts with [`VOLUME_PREFIX`]. The device is returned raw
/// (`/dev/disk5s1`); dropping the slice suffix for the whole-disk id is not
/// done here.
#[must_use]
pub fn parse_mounts(text: &str) -> Vec<(String, String)> {
    parse_mount_rows(text)
        .into_iter()
        .filter(|(_, mount)| last_component(mount).starts_with(VOLUME_PREFIX))
        .collect()
}

/// `(mount point, size in GiB)` pairs from `df -g` output, keeping rows
/// whose mount path's final component starts with [`VOLUME_PREFIX`]; the
/// size is the 1G-blocks column. The header row drops out on the numeric
/// parse.
#[must_use]
pub fn parse_df_gib(text: &str) -> Vec<(String, u64)> {
    parse_df_rows(text)
        .into_iter()
        .filter(|(mount, _)| last_component(mount).starts_with(VOLUME_PREFIX))
        .collect()
}

/// Every well-shaped `(device, mount point)` row from `mount` output.
pub(crate) fn parse_mount_rows(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (device, rest) = line.split_once(" on ")?;
            let (mount, _) = rest.split_once(" (")?;
            Some((device.to_string(), mount.to_string()))
        })
        .collect()
}

/// Every well-shaped `(mount point, size in GiB)` row from `df -g` output.
fn parse_df_rows(text: &str) -> Vec<(String, u64)> {
    text.lines()
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let size = match cols.get(1)?.parse::<u64>() {
                Ok(size) => size,
                Err(_) => return None,
            };
            Some((cols.last()?.to_string(), size))
        })
        .collect()
}

/// The final `/`-separated path component (`""` for a root path).
fn last_component(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or_default()
}

/// One command line: `argv[0]` is the program, the rest its arguments, and
/// `timeout_secs` bounds the call. Standard output comes back on success; a
/// non-zero exit is an `Err` carrying the tail of what was said.
pub trait Runner: Send + Sync {
    /// # Errors
    /// Returns an error when the program cannot start, exceeds `timeout_secs`,
    /// or exits non-zero.
    fn run(&self, argv: &[&str], timeout_secs: u64) -> Result<String, String>;
}

/// The real `hdiutil` and `diskutil` binaries on this machine.
#[derive(Debug, Clone, Copy)]
pub struct RealRunner;

impl Runner for RealRunner {
    fn run(&self, argv: &[&str], timeout_secs: u64) -> Result<String, String> {
        if !cfg!(target_os = "macos") {
            return Err(NOT_MACOS.to_string());
        }
        let Some((bin, args)) = argv.split_first() else {
            return Err("ramdisk: empty command".to_string());
        };
        let mut command = Command::new(bin);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| format!("ramdisk: cannot run `{bin}`: {e}"))?;
        let out = child.stdout.take().map(drain);
        let err = child.stderr.take().map(drain);
        let status = wait(&mut child, Duration::from_secs(timeout_secs), bin)?;
        let stdout = join(out);
        let stderr = join(err);
        if status.success() {
            Ok(stdout)
        } else {
            Err(tail(&format!("{stdout}\n{stderr}")))
        }
    }
}

/// One attached RAM disk: its mounted APFS volume device, optional physical
/// backing disk, volume name, size, and mount point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    /// The logical APFS device that appears in `mount` (for example, disk9s1).
    pub device: String,
    /// The whole disk hdiutil attached (for example, disk8).
    pub backing_device: Option<String>,
    pub volume: String,
    pub size_gib: u64,
    pub mount: PathBuf,
}

/// Create a `gib` GiB RAM disk named `name` at `at`: attach, erase as APFS,
/// unmount, then mount on `at`. A step that fails ejects the whole disk on a
/// best-effort basis and returns the failure, so a half-built disk never stays
/// attached.
///
/// # Errors
/// Returns an error when the mount directory cannot be created, when any
/// `hdiutil` / `diskutil` step refuses, or when the runner itself fails.
pub fn mount(runner: &dyn Runner, name: &str, gib: u64, at: &Path) -> Result<Disk, String> {
    std::fs::create_dir_all(at)
        .map_err(|e| format!("ramdisk: cannot create {}: {e}", at.display()))?;
    let raw = attach(runner, gib)?;
    let device = whole_disk(&raw);
    let volume_device = match prepare(runner, name, &device, at) {
        Ok(device) => device,
        Err(error) => {
            eject(runner, &device);
            return Err(error);
        }
    };
    Ok(Disk {
        device: volume_device,
        backing_device: Some(device),
        volume: name.to_string(),
        size_gib: gib,
        mount: at.to_path_buf(),
    })
}

/// Eject `disk`, then verify through `mount` that neither its device nor its
/// volume mount remains. A surviving disk is an error the caller logs loudly:
/// the job's memory was not released.
///
/// # Errors
/// Returns `<volume> still attached after eject` when the disk survived, the
/// eject failure when the disk is gone but `diskutil eject` refused, and the
/// runner's error when the verification listing cannot run.
pub fn teardown(runner: &dyn Runner, disk: &Disk) -> Result<(), String> {
    let actual_backing = backing_device_for_volume(runner, &disk.device, &disk.volume)?;
    if let Some(expected) = disk.backing_device.as_deref()
        && whole_disk(expected) != whole_disk(&actual_backing)
    {
        return Err(format!(
            "ramdisk: volume {} is no longer backed by owned disk {expected}",
            disk.volume
        ));
    }
    let device = whole_disk(&actual_backing);
    let eject = runner.run(&["diskutil", "eject", &device], DISKUTIL_SECS);
    if still_attached(runner, disk, &device)? {
        return Err(format!("{} still attached after eject", disk.volume));
    }
    eject.map(drop)
}

/// The RAM disks attached now: `mount` supplies the device and mount point,
/// `df -g` the size. A disk missing from the `df` listing reports zero.
#[must_use]
pub fn mounted(runner: &dyn Runner) -> Vec<Disk> {
    mounted_checked(runner).unwrap_or_default()
}

/// The attached RAM disks, failing if either operating-system listing failed.
/// Admission uses this form so a listing error cannot admit another disk
/// while existing memory use is unknown.
pub fn mounted_checked(runner: &dyn Runner) -> Result<Vec<Disk>, String> {
    let mounts = parse_mount_rows(&runner.run(&["mount"], LIST_SECS)?);
    let sizes = parse_df_rows(&runner.run(&["df", "-g"], LIST_SECS)?);
    Ok(mounts
        .into_iter()
        .filter_map(|(device, mount)| {
            let mount_path = PathBuf::from(&mount);
            let volume = if last_component(&mount).starts_with(VOLUME_PREFIX) {
                Some((last_component(&mount).to_string(), None))
            } else {
                pool_marker(&mount_path, &device)
            }?;
            let size_gib = sizes
                .iter()
                .find(|(path, _)| path == &mount)
                .map(|(_, gib)| *gib)
                .or(volume.1)
                .unwrap_or_default();
            Some(Disk {
                device,
                backing_device: None,
                volume: volume.0,
                size_gib,
                mount: mount_path,
            })
        })
        .collect())
}

/// RAM disks mounted over a local pool target cache do not have a `vivi-*`
/// mountpoint basename. Their exact slot marker carries the volume identity,
/// configured size, and mount path. A stale or mismatched marker is ignored.
fn pool_marker(mount: &Path, device: &str) -> Option<(String, Option<u64>)> {
    let slot = pool_slot_for_mount(mount)?;
    let marker_path = slot.join("ramdisk");
    if !marker_path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_file())
    {
        return None;
    }
    let Ok(marker) = std::fs::read_to_string(marker_path) else {
        return None;
    };
    let field = |name: &str| {
        marker
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
    };
    let volume = field("volume")?;
    let marked_device = field("device")?;
    let marked_volume_device = field("volume_device")?;
    let marked_mount = field("mount")?;
    let job = field("job")?;
    if !volume.starts_with(VOLUME_PREFIX)
        || job.is_empty()
        || !same_device_id(marked_device, marked_device)
        || !same_device_id(marked_volume_device, device)
        || marked_mount != mount.to_string_lossy()
    {
        return None;
    }
    Some((
        volume.to_string(),
        field("size_gib").and_then(|size| match size.parse::<u64>() {
            Ok(size) => Some(size),
            Err(_) => None,
        }),
    ))
}

pub(crate) fn same_device_id(left: &str, right: &str) -> bool {
    normalized_disk_id(left)
        .is_some_and(|left| normalized_disk_id(right).is_some_and(|right| left == right))
}

/// Slot root for a RAM mount at `<slot>/targets/<key>`. Root `repo` is the
/// seeded checkout and is never a RAM mount point.
pub(crate) fn pool_slot_for_mount(mount: &Path) -> Option<PathBuf> {
    let targets = mount.parent()?;
    (targets.file_name().is_some_and(|name| name == "targets"))
        .then(|| targets.parent().map(Path::to_path_buf))
        .flatten()
}

/// Attach an unformatted RAM disk of `gib` GiB and return its device (the
/// first line of `hdiutil`'s output).
fn attach(runner: &dyn Runner, gib: u64) -> Result<String, String> {
    let sectors = format!("ram://{}", sectors(gib));
    let out = runner.run(&["hdiutil", "attach", "-nomount", &sectors], ATTACH_SECS)?;
    let device = out.lines().next().unwrap_or_default().trim().to_string();
    if !device.starts_with("/dev/") {
        return Err(format!(
            "ramdisk: hdiutil attach returned no device: {}",
            out.trim()
        ));
    }
    Ok(device)
}

/// Shape an attached disk: erase it as APFS `name`, unmount it, then mount it
/// at `at`.
fn prepare(runner: &dyn Runner, name: &str, device: &str, at: &Path) -> Result<String, String> {
    runner.run(
        &["diskutil", "eraseVolume", "APFS", name, device],
        DISKUTIL_SECS,
    )?;
    let volume = resolve_apfs_volume(runner, name, device)?;
    if volume.mounted {
        runner.run(&["diskutil", "unmount", &volume.device], DISKUTIL_SECS)?;
    }
    let at = at.to_string_lossy().into_owned();
    runner
        .run(
            &["diskutil", "mount", "-mountPoint", &at, &volume.device],
            DISKUTIL_SECS,
        )
        .map(|_| volume.device)
}

struct ApfsVolume {
    device: String,
    mounted: bool,
}

/// Resolve the exact APFS volume created on `device`; APFS synthesizes a
/// logical device identifier that is not necessarily `<physical>s1`.
fn resolve_apfs_volume(
    runner: &dyn Runner,
    name: &str,
    device: &str,
) -> Result<ApfsVolume, String> {
    let physical = normalized_disk_id(device)
        .ok_or_else(|| format!("ramdisk: invalid attached device `{device}`"))?;
    if whole_disk(&physical) != physical {
        return Err(format!(
            "ramdisk: attached device is not a whole disk: `{device}`"
        ));
    }
    let container = attached_container(runner, device, &physical)?;
    let volumes = attached_container_volumes(runner, &container, &physical)?;
    let volume_id = volume_device(&volumes, name, &container)?;
    let logical_device = format!("/dev/{volume_id}");
    let mounted = volume_mount_state(runner, name, &logical_device, &volume_id, &container)?;
    Ok(ApfsVolume {
        device: logical_device,
        mounted,
    })
}

fn attached_container(runner: &dyn Runner, device: &str, physical: &str) -> Result<String, String> {
    let info_xml = runner.run(&["diskutil", "info", "-plist", device], DISKUTIL_SECS)?;
    let info = plist_json(runner, &info_xml, "disk info")?;
    let reported_physical = plist_string(&info, "DeviceIdentifier")
        .and_then(normalized_disk_id)
        .filter(|id| whole_disk(id) == physical);
    if reported_physical.is_none() {
        return Err(format!(
            "ramdisk: disk info does not identify attached disk `{physical}`"
        ));
    }
    let raw_container = plist_string(&info, "APFSContainerReference")
        .ok_or_else(|| "ramdisk: disk info has no APFS container reference".to_string())?;
    let container = normalized_disk_id(raw_container)
        .filter(|id| whole_disk(id) == *id)
        .ok_or_else(|| "ramdisk: disk info has an invalid APFS container reference".to_string())?;
    Ok(container)
}

fn attached_container_volumes(
    runner: &dyn Runner,
    container: &str,
    physical: &str,
) -> Result<Vec<Value>, String> {
    let apfs_xml = runner.run(
        &["diskutil", "apfs", "list", "-plist", &container],
        DISKUTIL_SECS,
    )?;
    let apfs = plist_json(runner, &apfs_xml, "APFS container list")?;
    let containers = apfs
        .get("Containers")
        .and_then(Value::as_array)
        .ok_or_else(|| "ramdisk: APFS list has no Containers array".to_string())?;
    let container_info = matching_container(containers, container)?;
    if !container_backed_by(container_info, physical) {
        return Err(format!(
            "ramdisk: APFS container `{container}` is not backed by attached disk `{physical}`"
        ));
    }
    container_info
        .get("Volumes")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| format!("ramdisk: APFS container `{container}` has no Volumes"))
}

fn matching_container<'a>(containers: &'a [Value], container: &str) -> Result<&'a Value, String> {
    let matching_containers: Vec<&Value> = containers
        .iter()
        .filter(|entry| {
            plist_string(entry, "ContainerReference")
                .and_then(normalized_disk_id)
                .is_some_and(|id| whole_disk(&id) == container)
        })
        .collect();
    match matching_containers.as_slice() {
        [container_info] => Ok(*container_info),
        _ => Err(format!(
            "ramdisk: APFS container `{container}` is missing or ambiguous"
        )),
    }
}

fn container_backed_by(container_info: &Value, physical: &str) -> bool {
    physical_store_ids(container_info).is_some_and(|ids| ids.len() == 1 && ids[0] == physical)
}

fn volume_device(volumes: &[Value], name: &str, container: &str) -> Result<String, String> {
    let matching_volumes: Vec<&Value> = volumes
        .iter()
        .filter(|volume| plist_string(volume, "Name") == Some(name))
        .collect();
    let volume = match matching_volumes.as_slice() {
        [volume] => *volume,
        _ => {
            return Err(format!(
                "ramdisk: APFS volume `{name}` is missing or ambiguous"
            ));
        }
    };
    let raw_volume_device = plist_string(volume, "DeviceIdentifier")
        .ok_or_else(|| format!("ramdisk: APFS volume `{name}` has no device identifier"))?;
    let volume_id = normalized_disk_id(raw_volume_device)
        .ok_or_else(|| format!("ramdisk: APFS volume `{name}` has an invalid device identifier"))?;
    if whole_disk(&volume_id) != container {
        return Err(format!(
            "ramdisk: APFS volume `{name}` does not belong to container `{container}`"
        ));
    }
    Ok(volume_id)
}

fn volume_mount_state(
    runner: &dyn Runner,
    name: &str,
    logical_device: &str,
    volume_id: &str,
    container: &str,
) -> Result<bool, String> {
    let volume_info_xml = runner.run(
        &["diskutil", "info", "-plist", &logical_device],
        DISKUTIL_SECS,
    )?;
    let volume_info = plist_json(runner, &volume_info_xml, "APFS volume info")?;
    let reported_volume =
        plist_string(&volume_info, "DeviceIdentifier").and_then(normalized_disk_id);
    let reported_container = plist_string(&volume_info, "APFSContainerReference")
        .and_then(normalized_disk_id)
        .filter(|id| whole_disk(id) == container);
    if reported_volume.as_deref() != Some(volume_id) || reported_container.is_none() {
        return Err(format!(
            "ramdisk: volume info does not confirm APFS volume `{name}` on `{container}`"
        ));
    }
    let mounted = match volume_info.get("MountPoint") {
        None | Some(Value::Null) => false,
        Some(Value::String(path)) => !path.is_empty(),
        Some(_) => {
            return Err(format!(
                "ramdisk: APFS volume `{name}` has a malformed MountPoint"
            ));
        }
    };
    Ok(mounted)
}

/// Resolve a mounted logical APFS volume back to its unique whole-disk store.
pub(crate) fn backing_device_for_volume(
    runner: &dyn Runner,
    logical_device: &str,
    name: &str,
) -> Result<String, String> {
    let info_xml = runner.run(
        &["diskutil", "info", "-plist", logical_device],
        DISKUTIL_SECS,
    )?;
    let info = plist_json(runner, &info_xml, "attached volume info")?;
    let actual_id = plist_string(&info, "DeviceIdentifier")
        .and_then(normalized_disk_id)
        .ok_or_else(|| format!("ramdisk: attached volume `{name}` has no valid device ID"))?;
    if !same_device_id(&actual_id, logical_device) {
        return Err(format!(
            "ramdisk: attached volume info does not match `{logical_device}`"
        ));
    }
    let raw_container = plist_string(&info, "APFSContainerReference")
        .ok_or_else(|| format!("ramdisk: attached volume `{name}` has no APFS container"))?;
    let container = normalized_disk_id(raw_container)
        .filter(|id| whole_disk(id) == *id)
        .ok_or_else(|| format!("ramdisk: attached volume `{name}` has an invalid container"))?;
    let apfs_xml = runner.run(
        &["diskutil", "apfs", "list", "-plist", &container],
        DISKUTIL_SECS,
    )?;
    let apfs = plist_json(runner, &apfs_xml, "attached APFS container")?;
    let containers = apfs
        .get("Containers")
        .and_then(Value::as_array)
        .ok_or_else(|| "ramdisk: APFS list has no Containers array".to_string())?;
    let container_info = matching_container(containers, &container)?;
    let volumes = container_info
        .get("Volumes")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("ramdisk: APFS container `{container}` has no Volumes"))?;
    let listed_volume = volume_device(volumes, name, &container)?;
    if !same_device_id(&listed_volume, logical_device) {
        return Err(format!(
            "ramdisk: volume `{name}` is no longer on `{logical_device}`"
        ));
    }
    unique_physical_store(container_info, &container, name)
}

fn unique_physical_store(
    container_info: &Value,
    container: &str,
    name: &str,
) -> Result<String, String> {
    let physical_ids = physical_store_ids(container_info)
        .ok_or_else(|| format!("ramdisk: APFS container `{container}` has no PhysicalStores"))?;
    match physical_ids.as_slice() {
        [physical] => Ok(format!("/dev/{physical}")),
        _ => Err(format!(
            "ramdisk: APFS volume `{name}` has no unique physical store"
        )),
    }
}

fn physical_store_ids(container_info: &Value) -> Option<Vec<String>> {
    let stores = container_info
        .get("PhysicalStores")
        .and_then(Value::as_array)?;
    let mut physical_ids: Vec<String> = stores
        .iter()
        .filter_map(|store| plist_string(store, "DeviceIdentifier"))
        .filter_map(normalized_disk_id)
        .map(|store| normalized_disk_id(&whole_disk(&store)))
        .flatten()
        .collect();
    physical_ids.sort();
    physical_ids.dedup();
    Some(physical_ids)
}

/// Convert a diskutil XML plist through macOS `plutil` and parse its JSON.
pub(crate) fn plist_json(
    runner: &dyn Runner,
    xml: &str,
    description: &str,
) -> Result<Value, String> {
    let mut file = tempfile::Builder::new()
        .prefix("vivi-ramdisk-")
        .suffix(".plist")
        .tempfile()
        .map_err(|error| format!("ramdisk: cannot stage {description} plist: {error}"))?;
    file.write_all(xml.as_bytes())
        .map_err(|error| format!("ramdisk: cannot write {description} plist: {error}"))?;
    let path = file.path().to_string_lossy().into_owned();
    let json = runner.run(
        &["/usr/bin/plutil", "-convert", "json", "-o", "-", &path],
        DISKUTIL_SECS,
    )?;
    serde_json::from_str(&json)
        .map_err(|error| format!("ramdisk: cannot parse {description} plist JSON: {error}"))
}

pub(crate) fn plist_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Normalize a diskutil device identifier to `diskN[sN...]`, rejecting other
/// text before it can become a command argument.
pub(crate) fn normalized_disk_id(device: &str) -> Option<String> {
    let identifier = device.strip_prefix("/dev/").unwrap_or(device);
    let suffix = identifier.strip_prefix("disk")?;
    let mut components = suffix.split('s');
    let base = components.next()?;
    if base.is_empty() || !base.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if components.any(|component| {
        component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        return None;
    }
    Some(identifier.to_string())
}

/// Acquire Vivi's per-user host-wide RAM admission lock. The lock directory
/// is shared by local projects and private to the owning account.
pub(crate) fn lock_admission() -> Result<File, String> {
    let file = open_admission_lock_file()?;
    file.lock()
        .map_err(|error| format!("ramdisk: cannot lock host admission: {error}"))?;
    Ok(file)
}

#[cfg(test)]
pub(crate) fn try_lock_admission() -> Result<Option<File>, String> {
    let file = open_admission_lock_file()?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => {
            Err(format!("ramdisk: cannot try host admission lock: {error}"))
        }
    }
}

fn open_admission_lock_file() -> Result<File, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

        let (lock_dir, uid) = admission_lock_directory()?;
        let path = lock_dir.join("admission.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(nofollow_flag())
            .open(&path)
            .map_err(|error| format!("ramdisk: cannot open admission lock: {error}"))?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("ramdisk: cannot inspect admission lock: {error}"))?;
        if !metadata.is_file()
            || metadata.uid() != uid
            || metadata.nlink() != 1
            || metadata.mode() & 0o600 != 0o600
            || metadata.mode() & 0o077 != 0
        {
            return Err("ramdisk: admission lock is not a privately owned file".to_string());
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        Err("ramdisk: host admission lock requires a Unix filesystem".to_string())
    }
}

#[cfg(unix)]
fn admission_lock_directory() -> Result<(PathBuf, u32), String> {
    use std::os::unix::fs::MetadataExt;

    #[cfg(target_os = "macos")]
    let temp = PathBuf::from("/private/tmp");
    #[cfg(target_os = "linux")]
    let temp = PathBuf::from("/tmp");
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let temp = std::env::temp_dir();
    let temp = fs::canonicalize(&temp)
        .map_err(|error| format!("ramdisk: cannot resolve temp directory: {error}"))?;
    let temp_metadata = fs::symlink_metadata(&temp)
        .map_err(|error| format!("ramdisk: cannot inspect temp directory: {error}"))?;
    if !temp_metadata.is_dir()
        || temp_metadata.file_type().is_symlink()
        || (temp_metadata.mode() & 0o002 != 0 && temp_metadata.mode() & 0o1000 == 0)
    {
        return Err("ramdisk: temp path is not a safe directory".to_string());
    }
    create_admission_lock_directory(&temp)
}

#[cfg(unix)]
fn create_admission_lock_directory(temp: &Path) -> Result<(PathBuf, u32), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};

    let home = dirs::home_dir().ok_or_else(|| "ramdisk: home directory is unknown".to_string())?;
    let uid = fs::metadata(&home)
        .map_err(|error| format!("ramdisk: cannot inspect home directory: {error}"))?
        .uid();
    let lock_dir = temp.join(format!("vivi-ramdisk-{uid}"));
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(&lock_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("ramdisk: cannot create lock directory: {error}")),
    }
    let metadata = fs::symlink_metadata(&lock_dir)
        .map_err(|error| format!("ramdisk: cannot inspect lock directory: {error}"))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != uid
        || metadata.mode() & 0o700 != 0o700
        || metadata.mode() & 0o077 != 0
    {
        return Err("ramdisk: lock directory is not privately owned".to_string());
    }
    Ok((lock_dir, uid))
}

#[cfg(target_os = "macos")]
const fn nofollow_flag() -> i32 {
    0x100
}

#[cfg(target_os = "linux")]
const fn nofollow_flag() -> i32 {
    0o400000
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
const fn nofollow_flag() -> i32 {
    0x100
}

/// Best-effort eject of `device`; a failure is left to the caller's error.
fn eject(runner: &dyn Runner, device: &str) {
    drop(runner.run(&["diskutil", "eject", device], DISKUTIL_SECS));
}

/// Whether the exact logical volume remains mounted on its recorded backing.
fn still_attached(runner: &dyn Runner, disk: &Disk, backing: &str) -> Result<bool, String> {
    let text = runner.run(&["mount"], LIST_SECS)?;
    let expected_mount = disk.mount.to_string_lossy();
    for (mounted_device, mount) in parse_mount_rows(&text) {
        if !same_device_id(&mounted_device, &disk.device) {
            continue;
        }
        let location_matches = mount == expected_mount.as_ref()
            || last_component(&mount) == disk.volume
            || pool_marker(Path::new(&mount), &mounted_device)
                .is_some_and(|(mounted_volume, _)| mounted_volume == disk.volume);
        if !location_matches {
            continue;
        }
        let actual_backing = backing_device_for_volume(runner, &mounted_device, &disk.volume)?;
        if whole_disk(&actual_backing) == whole_disk(backing) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `device` with any `sN` slice suffix stripped (`/dev/disk7s1` ->
/// `/dev/disk7`, `/dev/disk5s1s1` -> `/dev/disk5`), which is what `diskutil
/// eject` takes.
pub(crate) fn whole_disk(device: &str) -> String {
    let mut out = device.to_string();
    loop {
        let cut = out.rfind('s').filter(|&pos| {
            let head = &out[..pos];
            let suffix = &out[pos + 1..];
            !suffix.is_empty()
                && suffix.bytes().all(|b| b.is_ascii_digit())
                && head.bytes().last().is_some_and(|b| b.is_ascii_digit())
                && head
                    .rsplit('/')
                    .next()
                    .is_some_and(|n| n.starts_with("disk"))
        });
        match cut {
            Some(pos) => out.truncate(pos),
            None => return out,
        }
    }
}

/// Read a child's pipe to the end on its own thread, so a chatty command
/// cannot fill the pipe while the parent waits.
fn drain(mut pipe: impl Read + Send + 'static) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).map(drop).unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// The text a [`drain`] thread read (`""` when the thread died).
fn join(handle: Option<JoinHandle<String>>) -> String {
    let Some(handle) = handle else {
        return String::new();
    };
    handle.join().unwrap_or_default()
}

/// Wait for `child`, killing it once `limit` passes; `bin` names the call in
/// the timeout error.
fn wait(child: &mut Child, limit: Duration, bin: &str) -> Result<ExitStatus, String> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("ramdisk: waiting for `{bin}` failed: {e}"))?
        {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            drop(child.kill());
            drop(child.wait());
            return Err(format!(
                "ramdisk: `{bin}` did not finish in {}s",
                limit.as_secs()
            ));
        }
        std::thread::sleep(POLL);
    }
}

/// The last few non-blank lines of what a command said, on one short line.
fn tail(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let start = lines.len().saturating_sub(5);
    let joined = lines
        .get(start..)
        .map_or_else(String::new, |tail| tail.join("; "));
    let mut short: String = joined.chars().take(500).collect();
    if joined.chars().count() > 500 {
        short.push('…');
    }
    short
}

#[cfg(test)]
#[path = "ramdisk_test.rs"]
mod tests;
