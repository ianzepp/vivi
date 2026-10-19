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

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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
    let held = held_gib(runner);
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

/// Available memory in bytes, as `sysinfo` reports it right now. [`admit`]
/// reads it here; a caller that admits against a fixed memory reading (a
/// lane) takes it from this function instead of reading it twice.
///
/// `sysinfo`'s available figure is free plus inactive plus purgeable pages,
/// minus the compressor's; a machine whose compressed memory outweighs those
/// pages reports zero. The free-page count is the smaller, safe stand-in
/// then, so such a machine can still admit a job that fits its free memory.
#[must_use]
pub fn available_bytes() -> u64 {
    let system = System::new_with_specifics(
        RefreshKind::new().with_memory(MemoryRefreshKind::new().with_ram()),
    );
    match system.available_memory() {
        0 => system.free_memory(),
        available => available,
    }
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
    let mut mounts = Vec::new();
    for line in text.lines() {
        let Some((device, rest)) = line.split_once(" on ") else {
            continue;
        };
        let Some((mount, _options)) = rest.split_once(" (") else {
            continue;
        };
        if last_component(mount).starts_with(VOLUME_PREFIX) {
            mounts.push((device.to_string(), mount.to_string()));
        }
    }
    mounts
}

/// `(mount point, size in GiB)` pairs from `df -g` output, keeping rows
/// whose mount path's final component starts with [`VOLUME_PREFIX`]; the
/// size is the 1G-blocks column. The header row drops out on the numeric
/// parse.
#[must_use]
pub fn parse_df_gib(text: &str) -> Vec<(String, u64)> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let size: u64 = match cols.get(1).map(|col| col.parse::<u64>()) {
            Some(Ok(size)) => size,
            _ => continue,
        };
        let Some(mount) = cols.last() else {
            continue;
        };
        if last_component(mount).starts_with(VOLUME_PREFIX) {
            rows.push(((*mount).to_string(), size));
        }
    }
    rows
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

/// One attached RAM disk: the whole-disk device (`/dev/disk7`), its volume
/// name, its size, and where it is mounted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    pub device: String,
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
    if let Err(error) = prepare(runner, name, &device, at) {
        eject(runner, &device);
        return Err(error);
    }
    Ok(Disk {
        device,
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
    let device = whole_disk(&disk.device);
    let eject = runner.run(&["diskutil", "eject", &device], DISKUTIL_SECS);
    if still_attached(runner, &device, &disk.volume)? {
        return Err(format!("{} still attached after eject", disk.volume));
    }
    eject.map(drop)
}

/// The RAM disks attached now: `mount` supplies the device and mount point,
/// `df -g` the size. A disk missing from the `df` listing reports zero.
#[must_use]
pub fn mounted(runner: &dyn Runner) -> Vec<Disk> {
    let mounts = parse_mounts(&runner.run(&["mount"], LIST_SECS).unwrap_or_default());
    let sizes = parse_df_gib(&runner.run(&["df", "-g"], LIST_SECS).unwrap_or_default());
    mounts
        .into_iter()
        .map(|(device, mount)| {
            let size_gib = sizes
                .iter()
                .find(|(path, _)| path == &mount)
                .map_or(0, |(_, gib)| *gib);
            Disk {
                device,
                volume: last_component(&mount).to_string(),
                size_gib,
                mount: PathBuf::from(mount),
            }
        })
        .collect()
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
fn prepare(runner: &dyn Runner, name: &str, device: &str, at: &Path) -> Result<(), String> {
    let slice = format!("{device}s1");
    runner.run(
        &["diskutil", "eraseVolume", "APFS", name, device],
        DISKUTIL_SECS,
    )?;
    runner.run(&["diskutil", "unmount", &slice], DISKUTIL_SECS)?;
    let at = at.to_string_lossy().into_owned();
    runner
        .run(
            &["diskutil", "mount", "-mountPoint", &at, &slice],
            DISKUTIL_SECS,
        )
        .map(drop)
}

/// Best-effort eject of `device`; a failure is left to the caller's error.
fn eject(runner: &dyn Runner, device: &str) {
    drop(runner.run(&["diskutil", "eject", device], DISKUTIL_SECS));
}

/// Whether `mount` still lists `device` (any slice of it) or a mount whose
/// final component is `volume`.
fn still_attached(runner: &dyn Runner, device: &str, volume: &str) -> Result<bool, String> {
    let text = runner.run(&["mount"], LIST_SECS)?;
    Ok(parse_mounts(&text).iter().any(|(mounted_device, mount)| {
        whole_disk(mounted_device) == device || last_component(mount) == volume
    }))
}

/// `device` with any `sN` slice suffix stripped (`/dev/disk7s1` ->
/// `/dev/disk7`, `/dev/disk5s1s1` -> `/dev/disk5`), which is what `diskutil
/// eject` takes.
fn whole_disk(device: &str) -> String {
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
