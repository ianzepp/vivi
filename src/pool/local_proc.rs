//! The processes of a directory slot on this machine, on macOS and on Linux.
//!
//! A slot's boot wrapper is started detached, as the leader of a process
//! group of its own, with its output in the slot directory. The slot runs
//! while that process is alive; stopping ends everything the slot started.
//!
//! - **Identity.** `boot.pid` holds the wrapper's process id and `boot.start`
//!   the time that process started (`ps` `lstart`). A process counts as the
//!   slot's boot only when both match, so a reused process id is never taken
//!   for it. A zombie is not alive.
//! - **What a stop reaches.** The members of the wrapper's process group, its
//!   descendants (by parent process id, from one `ps` listing), and, where
//!   `/proc` exists (Linux), every process whose environment carries the
//!   slot's tag [`TAG`]. The tag is inherited, so it also finds a process that
//!   left the group and was orphaned. macOS has no readable `/proc` and hides
//!   the environment of protected programs, so there a process that
//!   deliberately leaves both the group and the process tree (a double fork
//!   with its own session) is not found; the harness runner kills what the
//!   agent leaves behind in its own group when the agent ends.
//! - **Stopping.** The processes found are frozen with `STOP` until a listing
//!   finds no new one, then ended with `TERM` and `KILL`; a frozen process forks
//!   nothing, so no child escapes by being born after the listing.
//! - **Tools.** `bash` (the scripts, and its builtin `kill`) and `ps`, which
//!   both systems have. Nothing else: no `setsid`, no `/proc` on macOS.

use std::collections::BTreeSet;
use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::local_git;
use crate::error::VivariumError;

/// Files in the slot directory.
pub const BOOT_PID: &str = "boot.pid";
pub const BOOT_START: &str = "boot.start";
pub const EXIT_CODE: &str = "exit_code";
pub const BOOT_OUT: &str = "boot.out";
/// The environment name every process of a slot carries, valued with the
/// slot directory.
pub const TAG: &str = "VIVI_SLOT_ROOT";

/// What the wrapper runs (`bash -c`, in the slot directory): the slot's
/// environment, the boot script, the exit code, and then whatever the boot
/// left running in the group. It ignores hang-up like `nohup`.
const WRAPPER: &str = "trap '' HUP; set -a; . ./slot.env; set +a\nbash ./boot.sh; rc=$?\necho \"$rc\" > exit_code\nkill -KILL 0\nexit \"$rc\"\n";

/// How long a stop waits for each signal to take effect.
const STOP_WAIT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);
/// How many listings a stop makes while freezing the slot's processes.
const FREEZE_PASSES: usize = 20;

/// Children started by this process that may not have been waited for yet.
static SPAWNED: Mutex<Vec<Child>> = Mutex::new(Vec::new());

/// One process of a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub zombie: bool,
    /// The start time as `ps` prints it.
    pub started: String,
}

fn number(text: Option<&str>) -> Option<u32> {
    text?.parse::<u32>().into_iter().next()
}

/// Read the output of `ps -A -o pid=,ppid=,pgid=,stat=,lstart=`.
#[must_use]
pub fn parse_listing(text: &str) -> Vec<Proc> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let ids = [
                number(fields.next())?,
                number(fields.next())?,
                number(fields.next())?,
            ];
            let stat = fields.next()?;
            let started = fields.collect::<Vec<_>>().join(" ");
            Some(Proc {
                pid: ids[0],
                ppid: ids[1],
                pgid: ids[2],
                zombie: stat.starts_with('Z'),
                started,
            })
        })
        .collect()
}

fn ps_error(detail: &str) -> VivariumError {
    VivariumError::Message(format!("cannot list processes with `ps`: {detail}"))
}

/// Every process of the machine.
///
/// # Errors
/// Returns an error if `ps` cannot run.
pub fn listing() -> Result<Vec<Proc>, VivariumError> {
    let out = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,pgid=,stat=,lstart="])
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| ps_error(&e.to_string()))?;
    if !out.status.success() {
        return Err(ps_error(String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(parse_listing(&String::from_utf8_lossy(&out.stdout)))
}

/// The start time of the live (not zombie) process `pid`, or none.
#[must_use]
pub fn identity(pid: u32) -> Option<String> {
    let Ok(out) = Command::new("ps")
        .args(["-o", "stat=,lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .output()
    else {
        return None;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let (stat, started) = text.trim().split_once(char::is_whitespace)?;
    (out.status.success() && !stat.starts_with('Z')).then(|| started.trim().to_string())
}

fn read_trimmed(dir: &Path, name: &str) -> Option<String> {
    let Ok(text) = std::fs::read_to_string(dir.join(name)) else {
        return None;
    };
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// The process id of the slot's boot wrapper, when it is alive.
#[must_use]
pub fn boot_pid(dir: &Path) -> Option<u32> {
    let pid = number(read_trimmed(dir, BOOT_PID).as_deref())?;
    let now = identity(pid)?;
    match read_trimmed(dir, BOOT_START) {
        Some(recorded) => (recorded == now).then_some(pid),
        // A start in progress: the time is written just after the id.
        None => Some(pid),
    }
}

/// True while the slot's boot wrapper is alive.
#[must_use]
pub fn is_alive(dir: &Path) -> bool {
    boot_pid(dir).is_some()
}

/// Wait for children of this process that have ended, so they do not stay
/// zombies for as long as this process lives.
pub fn reap() {
    if let Ok(mut children) = SPAWNED.lock() {
        children.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
    }
}

fn start_error(dir: &Path, detail: &str) -> VivariumError {
    VivariumError::Message(format!("cannot start {}: {detail}", dir.display()))
}

/// Start the slot's boot script detached, unless it already runs. The
/// process outlives this one.
///
/// # Errors
/// Returns an error if the process cannot be started.
pub fn start(dir: &Path) -> Result<(), VivariumError> {
    let work = dir
        .canonicalize()
        .map_err(|e| start_error(dir, &e.to_string()))?;
    // Two starts at once (a dispatch and a collector) must not both launch.
    let _held = local_git::lock_path(&work)?;
    if is_alive(&work) {
        return Ok(());
    }
    for name in [EXIT_CODE, BOOT_PID, BOOT_START] {
        remove_if_there(&work.join(name))?;
    }
    let out = File::create(work.join(BOOT_OUT)).map_err(|e| start_error(dir, &e.to_string()))?;
    let err = out
        .try_clone()
        .map_err(|e| start_error(dir, &e.to_string()))?;
    let child = Command::new("bash")
        .args(["-c", WRAPPER])
        .current_dir(&work)
        .env("WORK", &work)
        .env(TAG, &work)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0)
        .spawn()
        .map_err(|e| start_error(dir, &format!("failed to run `bash`: {e}")))?;
    let pid = child.id();
    if let Ok(mut spawned) = SPAWNED.lock() {
        spawned.push(child);
    }
    reap();
    write_identity(&work, pid)
}

fn write_identity(dir: &Path, pid: u32) -> Result<(), VivariumError> {
    std::fs::write(dir.join(BOOT_PID), format!("{pid}\n"))
        .map_err(|e| start_error(dir, &e.to_string()))?;
    // A boot that ended at once has no identity to record: it is simply not alive.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if let Some(started) = identity(pid) {
            return std::fs::write(dir.join(BOOT_START), format!("{started}\n"))
                .map_err(|e| start_error(dir, &e.to_string()));
        }
        if std::fs::metadata(dir.join(EXIT_CODE)).is_ok() {
            break;
        }
        std::thread::sleep(POLL);
    }
    Ok(())
}

fn remove_if_there(path: &Path) -> Result<(), VivariumError> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(VivariumError::Message(format!(
            "cannot remove {}: {e}",
            path.display()
        ))),
        _ => Ok(()),
    }
}

/// Everything in `listing` the slot started: the boot wrapper's process
/// group, its descendants, and the tagged processes. Zombies and this
/// process are never in it.
#[must_use]
pub fn slot_pids(dir: &Path, listing: &[Proc]) -> BTreeSet<u32> {
    let mut found = BTreeSet::new();
    if let Some(boot) = boot_pid(dir) {
        found.insert(boot);
        found.extend(listing.iter().filter(|p| p.pgid == boot).map(|p| p.pid));
        found.extend(descendants(boot, listing));
    }
    found.extend(tagged(dir));
    let live: BTreeSet<u32> = listing
        .iter()
        .filter(|p| !p.zombie)
        .map(|p| p.pid)
        .collect();
    found.retain(|pid| live.contains(pid) && *pid != std::process::id());
    found
}

/// The processes below `root` by parent process id.
#[must_use]
pub fn descendants(root: u32, listing: &[Proc]) -> BTreeSet<u32> {
    let mut found = BTreeSet::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for child in listing.iter().filter(|p| p.ppid == parent) {
            if found.insert(child.pid) {
                frontier.push(child.pid);
            }
        }
    }
    found
}

/// Processes whose environment carries the slot's tag. Only Linux can read
/// another process's environment from `/proc`.
#[cfg(target_os = "linux")]
fn tagged(dir: &Path) -> BTreeSet<u32> {
    let wanted = format!("{TAG}={}", dir.display());
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = number(entry.file_name().to_str())?;
            let Ok(env) = std::fs::read(entry.path().join("environ")) else {
                return None;
            };
            env.split(|b| *b == 0)
                .any(|item| item == wanted.as_bytes())
                .then_some(pid)
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn tagged(_dir: &Path) -> BTreeSet<u32> {
    BTreeSet::new()
}

/// Send `signal` (`TERM`, `KILL`) to `pids`, through the shell's builtin.
fn signal(name: &str, pids: &BTreeSet<u32>) {
    let args = pids.iter().map(u32::to_string);
    let status = Command::new("bash")
        .args(["-c", "kill -s \"$0\" \"$@\" 2>/dev/null; exit 0", name])
        .args(args)
        .stdin(Stdio::null())
        .status();
    if let Err(e) = status {
        eprintln!("warning: could not send {name}: {e}");
    }
}

/// Freeze the slot's processes with `STOP` until a listing finds no new one.
/// A frozen process starts nothing, so a parent that is ended next cannot
/// leave behind a child nobody listed.
fn freeze(work: &Path) -> Result<BTreeSet<u32>, VivariumError> {
    let mut frozen = BTreeSet::new();
    for _ in 0..FREEZE_PASSES {
        let fresh: BTreeSet<u32> = slot_pids(work, &listing()?)
            .difference(&frozen)
            .copied()
            .collect();
        if fresh.is_empty() {
            break;
        }
        signal("STOP", &fresh);
        frozen.extend(fresh);
    }
    Ok(frozen)
}

/// End everything the slot started: freeze it, ask with `TERM` (and let the
/// frozen processes hear it with `CONT`), then insist with `KILL`, and forget
/// the process files.
///
/// # Errors
/// Returns an error if a process survives both signals.
pub fn stop(dir: &Path) -> Result<(), VivariumError> {
    let work = dir
        .canonicalize()
        .map_err(|e| start_error(dir, &e.to_string()))?;
    let frozen = freeze(&work)?;
    let mut pids = slot_pids(&work, &listing()?);
    for name in ["TERM", "KILL"] {
        if pids.is_empty() {
            break;
        }
        signal(name, &pids);
        signal("CONT", &frozen);
        let deadline = Instant::now() + STOP_WAIT;
        loop {
            pids = slot_pids(&work, &listing()?);
            if pids.is_empty() || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(POLL);
        }
    }
    if !pids.is_empty() {
        return Err(VivariumError::Message(format!(
            "could not stop processes of {}: {pids:?}",
            work.display()
        )));
    }
    reap();
    for name in [BOOT_PID, BOOT_START, EXIT_CODE] {
        remove_if_there(&work.join(name))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "local_proc_test.rs"]
mod tests;
