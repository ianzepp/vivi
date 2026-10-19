//! Live process status for a role binding.
//!
//! A role stores a `pid` + `host` binding (self-set, PID-file semantics).
//! This module computes liveness, CPU, memory, and uptime **fresh on each
//! call** from the OS via `sysinfo`. Nothing here is ever persisted.
//!
//! Cross-host honesty: if the stored `host` differs from the local host, we
//! refuse to probe the local process table and report [`ProcessState::Remote`]
//! instead of a false `Dead`.

use std::thread;
use std::time::Duration;

use serde::Serialize;
use sysinfo::{Pid, Process, ProcessStatus, ProcessesToUpdate, System};

use crate::role_schedule::ScheduleReport;

/// Minimum interval for a meaningful CPU delta. Matches sysinfo's
/// `MINIMUM_CPU_UPDATE_INTERVAL` on Linux; applied only when a live local pid
/// is probed.
const CPU_SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

/// The local hostname, or `None` if `sysinfo` cannot determine it.
#[must_use]
pub fn local_host_name() -> Option<String> {
    System::host_name()
}

/// Full status outcome for `vivi role status`: process binding plus schedule.
#[derive(Debug, Clone, Serialize)]
pub struct RoleStatusOutcome {
    pub name: String,
    pub address: String,
    pub pid: Option<u32>,
    pub host: Option<String>,
    pub status: ProcessReport,
    pub schedule: ScheduleReport,
}

/// Computed liveness and resource context. All fields are observed, never stored.
#[derive(Debug, Clone, Serialize)]
pub struct ProcessReport {
    pub state: ProcessState,
    /// `Some(true)` only when the process is confirmed alive on this host.
    /// `None` for `NotSet`, `Remote`, or `Unknown`.
    pub running: Option<bool>,
    pub name: Option<String>,
    pub cpu_percent: Option<f32>,
    pub memory_bytes: Option<u64>,
    pub uptime_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    /// No pid is bound to the role.
    NotSet,
    /// The pid lives on a different host; local liveness is unavailable.
    Remote,
    /// Process exists and is in a live state (runnable or sleeping).
    Alive,
    /// Terminated but not yet reaped by its parent.
    Zombie,
    /// No live process with this pid on this host.
    Dead,
    /// The role uses a subagent harness; OS PID is not a valid liveness signal.
    Subagent,
    /// Status could not be classified.
    Unknown,
}

/// Probe a role's bound process. Liveness is computed fresh; nothing is stored.
///
/// - `pid` `None` → [`ProcessState::NotSet`].
/// - `harness` is `subagent` → [`ProcessState::Subagent`] (PID is not a valid
///   liveness signal for one-shot tool shells).
/// - stored host differs from local → [`ProcessState::Remote`] (no local probe).
/// - otherwise the OS is probed via `sysinfo`.
///
/// Pays one [`CPU_SAMPLE_INTERVAL`] sleep for a live local pid so `cpu_percent`
/// reflects a real delta. Use [`probe_quick`] when many roles are scanned and a
/// precise CPU reading is not required.
#[must_use]
pub fn probe(pid: Option<u32>, stored_host: Option<&str>, harness: Option<&str>) -> ProcessReport {
    probe_with(pid, stored_host, harness, true)
}

/// Lightweight probe for scanning many roles (e.g. `vivi board --process`).
///
/// Same states as [`probe`] but skips the CPU sample sleep, so a live pid is
/// instant. `cpu_percent` is `None` here — a single refresh cannot honestly
/// report CPU. Precise CPU stays `vivi role status <name>`.
#[must_use]
pub fn probe_quick(
    pid: Option<u32>,
    stored_host: Option<&str>,
    harness: Option<&str>,
) -> ProcessReport {
    probe_with(pid, stored_host, harness, false)
}

fn probe_with(
    pid: Option<u32>,
    stored_host: Option<&str>,
    harness: Option<&str>,
    sample_cpu: bool,
) -> ProcessReport {
    if harness == Some(crate::mailspace::ROLE_HARNESS_SUBAGENT) {
        return subagent();
    }
    let Some(pid) = pid else {
        return not_set();
    };
    if host_is_remote(stored_host, local_host_name().as_deref()) {
        return remote();
    }
    probe_local(pid, sample_cpu)
}

fn host_is_remote(stored: Option<&str>, local: Option<&str>) -> bool {
    match (stored, local) {
        (Some(stored), Some(local)) => stored != local,
        // No stored host, or local hostname unreadable → assume local.
        _ => false,
    }
}

fn not_set() -> ProcessReport {
    ProcessReport {
        state: ProcessState::NotSet,
        running: None,
        name: None,
        cpu_percent: None,
        memory_bytes: None,
        uptime_seconds: None,
    }
}

fn remote() -> ProcessReport {
    ProcessReport {
        state: ProcessState::Remote,
        running: None,
        name: None,
        cpu_percent: None,
        memory_bytes: None,
        uptime_seconds: None,
    }
}

fn subagent() -> ProcessReport {
    ProcessReport {
        state: ProcessState::Subagent,
        running: None,
        name: None,
        cpu_percent: None,
        memory_bytes: None,
        uptime_seconds: None,
    }
}

fn dead() -> ProcessReport {
    ProcessReport {
        state: ProcessState::Dead,
        running: Some(false),
        name: None,
        cpu_percent: None,
        memory_bytes: None,
        uptime_seconds: None,
    }
}

/// Probe the local process table. When `sample_cpu` is set, a live pid pays
/// one [`CPU_SAMPLE_INTERVAL`] sleep so `cpu_usage()` reflects a real delta
/// rather than a zero first read.
fn probe_local(pid: u32, sample_cpu: bool) -> ProcessReport {
    let mut system = System::new();
    let target = Pid::from_u32(pid);
    refresh_one(&mut system, target);
    if system.process(target).is_none() {
        return dead();
    }
    if sample_cpu {
        thread::sleep(CPU_SAMPLE_INTERVAL);
        refresh_one(&mut system, target);
    }
    match system.process(target) {
        Some(process) => report_live(process, sample_cpu),
        None => dead(),
    }
}

fn refresh_one(system: &mut System, target: Pid) {
    system.refresh_processes(ProcessesToUpdate::Some(&[target]), false);
}

fn report_live(process: &Process, include_cpu: bool) -> ProcessReport {
    let state = classify(process.status());
    ProcessReport {
        running: match state {
            ProcessState::Alive => Some(true),
            ProcessState::Zombie | ProcessState::Dead => Some(false),
            _ => None,
        },
        state,
        name: Some(process.name().to_string_lossy().into_owned()),
        // A single refresh cannot honestly report CPU; only the sampled probe does.
        cpu_percent: include_cpu.then_some(process.cpu_usage()),
        memory_bytes: Some(process.memory()),
        uptime_seconds: Some(process.run_time()),
    }
}

fn classify(status: ProcessStatus) -> ProcessState {
    match status {
        ProcessStatus::Zombie => ProcessState::Zombie,
        ProcessStatus::Dead => ProcessState::Dead,
        ProcessStatus::Run
        | ProcessStatus::Sleep
        | ProcessStatus::Idle
        | ProcessStatus::Stop
        | ProcessStatus::Tracing
        | ProcessStatus::Wakekill
        | ProcessStatus::Waking
        | ProcessStatus::Parked
        | ProcessStatus::LockBlocked
        | ProcessStatus::UninterruptibleDiskSleep => ProcessState::Alive,
        ProcessStatus::Unknown(_) => ProcessState::Unknown,
    }
}

impl RoleStatusOutcome {
    /// Print a short human-readable status block.
    pub fn print_text(&self) {
        println!("{} {}", self.name, self.address);
        println!("  pid:     {}", self.pid_or_unset());
        match &self.host {
            Some(host) => println!("  host:    {host}"),
            None => println!("  host:    (unset)"),
        }
        println!("  state:   {}", state_label(&self.status.state));
        println!(
            "  running: {}",
            match self.status.running {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unknown",
            }
        );
        if let Some(name) = &self.status.name {
            println!("  process: {name}");
        }
        if let Some(cpu) = self.status.cpu_percent {
            println!("  cpu:     {cpu:.1}%");
        }
        if let Some(bytes) = self.status.memory_bytes {
            println!("  memory:  {}", format_bytes(bytes));
        }
        if let Some(secs) = self.status.uptime_seconds {
            println!("  uptime:  {}", format_duration(secs));
        }
        if let Some(note) = state_note(&self.status.state) {
            println!("  {note}");
        }
        self.print_schedule();
    }

    fn print_schedule(&self) {
        use crate::role_schedule::{ScheduleState, state_label};
        let schedule = &self.schedule;
        print!("  schedule: {}", state_label(schedule.state));
        if matches!(schedule.state, ScheduleState::None) {
            println!();
            return;
        }
        if let Some(cadence) = &schedule.cadence {
            print!("  cadence {cadence}");
        }
        if let Some(next_due) = &schedule.next_due_at {
            print!("  next due {next_due}");
        }
        if let Some(age) = schedule.age_seconds {
            print!("  last signal {}", format_duration(age));
            print!(" ago");
        } else if matches!(schedule.state, ScheduleState::Never) {
            print!("  (no outbound signal)");
        }
        if let Some(handle) = &schedule.last_signal_handle {
            print!("  {handle}");
        }
        println!();
    }

    fn pid_or_unset(&self) -> String {
        match self.pid {
            Some(pid) => pid.to_string(),
            None => "(unset)".into(),
        }
    }
}

fn state_label(state: &ProcessState) -> &'static str {
    match state {
        ProcessState::NotSet => "not_set",
        ProcessState::Remote => "remote",
        ProcessState::Alive => "alive",
        ProcessState::Zombie => "zombie",
        ProcessState::Dead => "dead",
        ProcessState::Subagent => "subagent",
        ProcessState::Unknown => "unknown",
    }
}

fn state_note(state: &ProcessState) -> Option<&'static str> {
    match state {
        ProcessState::NotSet => Some("no process binding set for this role"),
        ProcessState::Remote => {
            Some("pid lives on a different host; local liveness is unavailable")
        }
        ProcessState::Subagent => Some("subagent harness: OS pid is not a valid liveness signal"),
        ProcessState::Dead => Some("no live process with this pid on this host"),
        _ => None,
    }
}

// Precision loss from u64 -> f64 only matters above 4.5 PiB; irrelevant for
// process memory display.
#[allow(clippy::cast_precision_loss)]
fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let mins = (seconds % 3_600) / 60;
    let secs = seconds % 60;
    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m {secs}s")
    } else if mins > 0 {
        format!("{mins}m {secs}s")
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
#[path = "role_status_test.rs"]
mod tests;
