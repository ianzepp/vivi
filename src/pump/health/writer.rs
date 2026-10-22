//! Whether a pump process holds `<home>/lock`, found without taking it.
//!
//! The pump holds an exclusive `flock` on its lock file for its whole run.
//! [`crate::pump::run::lock_is_held`] answers by trying the lock itself, which
//! can make a pump starting at that instant fail. On Linux the kernel lists
//! every held `flock` in `/proc/locks` with the holder's pid and the file's
//! device and inode, so this probe only reads. Elsewhere it returns `None` and
//! the caller infers the writer from tick freshness.

use std::fs;
use std::path::Path;

/// One `FLOCK` row of `/proc/locks`: holder pid, device and inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LockRow {
    pub pid: u32,
    pub major: u64,
    pub minor: u64,
    pub inode: u64,
}

/// Where the probe looks; tests point it at a fixture tree.
pub(crate) struct ProcRoot<'a> {
    pub locks: &'a Path,
    pub proc_dir: &'a Path,
}

/// The pid holding `lock`'s flock, `Some(None)` when nobody holds it, and
/// `None` when `/proc/locks` cannot be read (not Linux).
#[must_use]
pub(crate) fn holder(lock: &Path) -> Option<Option<u32>> {
    holder_in(
        lock,
        &ProcRoot {
            locks: Path::new("/proc/locks"),
            proc_dir: Path::new("/proc"),
        },
    )
}

/// [`holder`] over an explicit `/proc`.
#[must_use]
pub(crate) fn holder_in(lock: &Path, proc_root: &ProcRoot<'_>) -> Option<Option<u32>> {
    let Ok(text) = fs::read_to_string(proc_root.locks) else {
        return None;
    };
    let Some((dev, inode)) = file_id(lock) else {
        // No lock file: no pump has ever run here, so none is running.
        return Some(None);
    };
    let (major, minor) = split_dev(dev);
    let found = parse_locks(&text)
        .into_iter()
        .filter(|row| row.inode == inode)
        .find(|row| {
            (row.major == major && row.minor == minor)
                || holds_open(proc_root.proc_dir, row.pid, lock)
        });
    Some(found.map(|row| row.pid))
}

/// The `FLOCK` rows of a `/proc/locks` text. Waiters (`->`) are skipped:
/// they want the lock, they do not hold it.
#[must_use]
pub(crate) fn parse_locks(text: &str) -> Vec<LockRow> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<LockRow> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.get(1) != Some(&"FLOCK") {
        return None;
    }
    let mut id = fields.get(5)?.split(':');
    let (major, minor, inode) = (id.next()?, id.next()?, id.next()?);
    parse_row(fields.get(4)?, major, minor, inode).unwrap_or(None)
}

/// The numeric fields of one row; a malformed number is no row.
fn parse_row(
    pid: &str,
    major: &str,
    minor: &str,
    inode: &str,
) -> Result<Option<LockRow>, std::num::ParseIntError> {
    Ok(Some(LockRow {
        pid: pid.parse()?,
        major: u64::from_str_radix(major, 16)?,
        minor: u64::from_str_radix(minor, 16)?,
        inode: inode.parse()?,
    }))
}

/// Linux `dev_t` to `(major, minor)`, as glibc's `gnu_dev_major/minor`.
#[must_use]
pub(crate) fn split_dev(dev: u64) -> (u64, u64) {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & 0xffff_f000);
    let minor = (dev & 0xff) | ((dev >> 12) & 0xffff_ff00);
    (major, minor)
}

#[cfg(unix)]
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = fs::metadata(path) else {
        return None;
    };
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_id(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// Whether process `pid` has `lock` open. Some filesystems (btrfs
/// subvolumes) report a different device to `stat` than to `/proc/locks`, so
/// an inode match on another device is confirmed through the holder's open
/// files. Unreadable `fd` entries (another user's process) never confirm.
fn holds_open(proc_dir: &Path, pid: u32, lock: &Path) -> bool {
    let Ok(target) = fs::canonicalize(lock) else {
        return false;
    };
    let Ok(entries) = fs::read_dir(proc_dir.join(pid.to_string()).join("fd")) else {
        return false;
    };
    entries
        .filter_map(Result::ok)
        .any(|entry| fs::read_link(entry.path()).is_ok_and(|link| link == target))
}
