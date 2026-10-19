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
//! Only the pure core lives here: no process spawning, no I/O. The
//! mount/teardown wrappers shell out to `hdiutil` and `diskutil` and feed
//! their output to [`parse_mounts`] and [`parse_df_gib`].

/// Every RAM disk volume name starts with this, so orphan sweeps and
/// parsers can recognize our disks.
pub const VOLUME_PREFIX: &str = "vivi-";

/// One gibibyte, in bytes.
const GIB: u64 = 1024 * 1024 * 1024;

/// The RAM disk volume name for `parts` (mailspace, slot, job): joined with
/// `-`, every `/` or whitespace replaced by `-`, prefixed `vivi-`.
#[must_use]
pub fn volume_name(parts: &[&str]) -> String {
    let sanitized: String = parts
        .join("-")
        .chars()
        .map(|c| if c == '/' || c.is_whitespace() { '-' } else { c })
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
        let Some(size) = cols.get(1).and_then(|col| col.parse::<u64>().ok()) else {
            continue;
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

#[cfg(test)]
#[path = "ramdisk_test.rs"]
mod tests;
