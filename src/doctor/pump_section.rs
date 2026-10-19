//! The pump section of `vivi doctor`: control mode, lock, tick age, service,
//! and the store-backed state: exceptions, quarantines, and the last gate.
//!
//! Checks are pure functions over [`PumpFacts`] and [`PumpStateFacts`]. The
//! gatherer alone reads the filesystem, probes the service manager, and opens
//! the pump store read-only.

use std::path::Path;
use std::time::SystemTime;

use super::Finding;
use crate::mailspace::Mailspace;
use crate::pump::config::PumpConfig;
use crate::pump::control::{PumpControl, PumpMode};
use crate::pump::state::{GateRun, State};
use crate::pump::store::{DB_FILENAME, PumpStore};

const SECTION: &str = "pump";
const HOME: &str = "pump home";
const CONTROL: &str = "control";
const LOCK: &str = "lock";
const TICK: &str = "tick";
const SERVICE: &str = "service";
const TICK_STALE_SECS: u64 = 2 * 3600;
const START_FIX: &str = "vivi pump start --project <root>";
const LOG_FIX: &str = "check the pump process and vivi pump log";

#[cfg(target_os = "macos")]
const SERVICE_LABEL: &str = "com.vivi.pump-service";
#[cfg(target_os = "linux")]
const SERVICE_UNIT: &str = "vivi-pump-service.service";

const EXCEPTIONS: &str = "open exceptions";
const QUARANTINE: &str = "quarantined batches";
const GATE: &str = "last gate";
const PUMP_STATE: &str = "pump state";
const GATE_STALE_SECS: u64 = 3600;
const MAX_SUMMARY_CHARS: usize = 120;
const EXCEPTIONS_FIX: &str =
    "vivi pump log --exceptions --project <root>, then requeue or repair";
const QUARANTINE_FIX: &str =
    "the pump mints repair units automatically; see vivi pump log --exceptions";
const GATE_STUCK_FIX: &str = "inspect .vivi/pump/gate-results and vivi pump log";
const GATE_FAILED_FIX: &str =
    "vivi pump once reruns the gate; gatefix repairs systemic failures";

/// What the gatherer learned about one project's pump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PumpFacts {
    pub home_exists: bool,
    pub mode: PumpMode,
    pub shutdown_requested: bool,
    pub lock_held: bool,
    pub tick_age_secs: Option<u64>,
    pub service: ServicePresence,
}

/// Whether the platform service manager knows a pump service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServicePresence {
    Installed,
    Missing,
    Unavailable,
}

/// Store-backed pump facts: open exceptions, quarantine branches, and the
/// last gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PumpStateFacts {
    /// `"<kind>: <text>"` per open exception row, oldest first.
    pub open_exceptions: Vec<String>,
    /// Quarantine branches pending repair in the integration clone.
    pub quarantines: usize,
    /// The doctor label for the last gate; `None` before any gate ran.
    pub last_gate: Option<String>,
}

/// Gather and report pump facts for a project, mirroring `pump run status`
/// and adding the store-backed state findings.
#[must_use]
pub fn findings(project: &Path) -> Vec<Finding> {
    let config = match Mailspace::discover(Some(project)) {
        Ok(mailspace) => mailspace.config.pump.unwrap_or_default(),
        Err(_) => PumpConfig::default(),
    };
    let home = config.home_dir(project);
    let mut report = if home.exists() {
        findings_from(&gather_facts(&home, probe_service))
    } else {
        findings_from(&missing_home_facts())
    };
    report.extend(store_findings(project, &config));
    report
}

/// The doctor findings for one set of pump facts. Pure; never probes.
#[must_use]
pub fn findings_from(facts: &PumpFacts) -> Vec<Finding> {
    if !facts.home_exists {
        return vec![Finding::skipped(
            SECTION,
            HOME,
            "pump not initialized for this project",
        )];
    }
    let mut findings = vec![control_finding(facts)];
    if let Some(lock) = lock_finding(facts) {
        findings.push(lock);
    }
    findings.push(tick_finding(facts));
    findings.push(service_finding(facts));
    findings
}

fn missing_home_facts() -> PumpFacts {
    PumpFacts {
        home_exists: false,
        mode: PumpMode::Running,
        shutdown_requested: false,
        lock_held: false,
        tick_age_secs: None,
        service: ServicePresence::Unavailable,
    }
}

/// The doctor findings for the pump's store-backed state: exceptions,
/// quarantines, and the last gate. Pure; never probes.
#[must_use]
pub fn state_findings_from(facts: &PumpStateFacts, now_epoch: u64) -> Vec<Finding> {
    vec![
        exceptions_finding(facts),
        quarantine_finding(facts),
        gate_finding(facts, now_epoch),
    ]
}

/// The store-backed findings for one project. One skip replaces all three
/// while the pump database does not exist yet.
fn store_findings(project: &Path, config: &PumpConfig) -> Vec<Finding> {
    match state_facts(project, config) {
        Some(facts) => state_findings_from(&facts, now_epoch()),
        None => vec![Finding::skipped(SECTION, PUMP_STATE, "no pump state yet")],
    }
}

/// Read the pump store and integration clone for the state facts; `None`
/// when the pump database does not exist yet.
fn state_facts(project: &Path, config: &PumpConfig) -> Option<PumpStateFacts> {
    let path = config.home_dir(project).join(DB_FILENAME);
    let store = PumpStore::open_read_only(&path).ok()??;
    let open_exceptions = store
        .exceptions()
        .unwrap_or_default()
        .into_iter()
        .map(|(_, kind, _, text)| format!("{kind}: {text}"))
        .collect();
    let state = store.load().ok()?;
    Some(PumpStateFacts {
        open_exceptions,
        quarantines: quarantine_count(&config.repo_dir(project)),
        last_gate: last_gate_label(&state),
    })
}

/// The label for a gate run proper: `"running since <epoch>"` while its task
/// is open, `"finished at <epoch>"` once it is not. The finished outcome
/// comes from the newest gate record; see `last_gate_label`.
fn gate_label(run: &GateRun) -> String {
    match run.task {
        Some(_) => format!("running since {}", run.at),
        None => format!("finished at {}", run.at),
    }
}

/// The `last_gate` fact for one loaded state: `None` before any gate ran,
/// otherwise the run's label with the newest gate record's outcome attached.
fn last_gate_label(state: &State) -> Option<String> {
    let run = &state.last_gate;
    if run.task.is_none() && state.gates.is_empty() {
        return None;
    }
    if run.task.is_some() {
        return Some(gate_label(run));
    }
    let record = state.gates.last()?;
    match record.names.as_deref() {
        Some(names) if names.is_empty() => Some(format!("passed at {}", record.at)),
        Some(names) => Some(format!(
            "failed at {}: {}",
            record.at,
            truncate(&names.join(", "))
        )),
        None => Some(format!("failed at {}", record.at)),
    }
}

/// Report the open exceptions from the pump store.
fn exceptions_finding(facts: &PumpStateFacts) -> Finding {
    let Some(first) = facts.open_exceptions.first() else {
        return Finding::ok(SECTION, EXCEPTIONS, "no open exceptions");
    };
    Finding::warn(
        SECTION,
        EXCEPTIONS,
        &format!(
            "{} exception(s): {}",
            facts.open_exceptions.len(),
            truncate(first)
        ),
        EXCEPTIONS_FIX,
    )
}

/// Report quarantine branches pending repair in the integration clone.
fn quarantine_finding(facts: &PumpStateFacts) -> Finding {
    if facts.quarantines == 0 {
        return Finding::ok(SECTION, QUARANTINE, "no quarantines");
    }
    Finding::warn(
        SECTION,
        QUARANTINE,
        &format!("{} quarantine branch(es) pending repair", facts.quarantines),
        QUARANTINE_FIX,
    )
}

/// Report the last gate: skipped before the first gate, stuck while a stale
/// run never finished, warn on the newest failure, ok once it passed.
fn gate_finding(facts: &PumpStateFacts, now_epoch: u64) -> Finding {
    let Some(label) = facts.last_gate.as_deref() else {
        return Finding::skipped(SECTION, GATE, "no gate has run");
    };
    if let Some(started) = label.strip_prefix("running since ") {
        let started: u64 = started.parse().unwrap_or(now_epoch);
        if now_epoch.saturating_sub(started) > GATE_STALE_SECS {
            return Finding::warn(SECTION, GATE, "gate stuck", GATE_STUCK_FIX);
        }
        return Finding::ok(SECTION, GATE, label);
    }
    if label.starts_with("failed") {
        return Finding::warn(
            SECTION,
            GATE,
            &format!("last gate {label}"),
            GATE_FAILED_FIX,
        );
    }
    Finding::ok(SECTION, GATE, &format!("last gate {label}"))
}

/// Count `refs/heads/pump/quarantine/*` branches in the integration clone.
/// A missing clone or missing git reports zero: the doctor never fails here.
fn quarantine_count(repo: &Path) -> usize {
    if !repo.is_dir() {
        return 0;
    }
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads/pump/quarantine/",
        ])
        .output();
    let Ok(output) = output else {
        return 0;
    };
    if !output.status.success() {
        return 0;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

/// Cut a summary to [`MAX_SUMMARY_CHARS`] characters, appending `...`.
fn truncate(text: &str) -> String {
    if text.chars().count() <= MAX_SUMMARY_CHARS {
        return text.to_string();
    }
    let cut: String = text.chars().take(MAX_SUMMARY_CHARS).collect();
    format!("{cut}...")
}

/// Whole seconds since the Unix epoch.
fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Probe the live pump. The service callback is injectable so callers and
/// tests can gather local facts without invoking a system command.
fn gather_facts(home: &Path, service_probe: impl FnOnce() -> ServicePresence) -> PumpFacts {
    let control = match crate::pump::control::inspect(home) {
        Ok(control) => control,
        Err(_) => PumpControl {
            mode: PumpMode::Running,
            shutdown_requested: false,
        },
    };
    PumpFacts {
        home_exists: true,
        mode: control.mode,
        shutdown_requested: control.shutdown_requested,
        lock_held: crate::pump::run::lock_is_held(home).unwrap_or(false),
        tick_age_secs: tick_age_secs(home),
        service: service_probe(),
    }
}

/// Report the operator control mode and a requested shutdown.
fn control_finding(facts: &PumpFacts) -> Finding {
    match (facts.mode, facts.shutdown_requested) {
        (PumpMode::Paused, _) => Finding::warn(
            SECTION,
            CONTROL,
            "pump is paused",
            "vivi pump resume --project <root>",
        ),
        (PumpMode::Stopped, _) => Finding::warn(SECTION, CONTROL, "pump is stopped", START_FIX),
        (PumpMode::Running, true) => {
            Finding::warn(SECTION, CONTROL, "shutdown requested", START_FIX)
        }
        (PumpMode::Running, false) => Finding::ok(SECTION, CONTROL, "control mode running"),
    }
}

/// Report whether a pump process owns its lock.
fn lock_finding(facts: &PumpFacts) -> Option<Finding> {
    if !facts.lock_held {
        return Some(Finding::warn(
            SECTION,
            LOCK,
            "no pump process holds the lock",
            "vivi pump run --project <root> (or vivi pump service install)",
        ));
    }
    if facts.mode == PumpMode::Running {
        return Some(Finding::ok(SECTION, LOCK, "pump running"));
    }
    None
}

/// Report whether `status.txt` is missing, stale, or fresh.
fn tick_finding(facts: &PumpFacts) -> Finding {
    match facts.tick_age_secs {
        None => Finding::warn(
            SECTION,
            TICK,
            "status.txt missing: the pump has never ticked",
            START_FIX,
        ),
        Some(age) if age > TICK_STALE_SECS => {
            Finding::warn(SECTION, TICK, &format!("last tick {age}s ago"), LOG_FIX)
        }
        Some(age) => Finding::ok(SECTION, TICK, &format!("last tick {age}s ago")),
    }
}

/// Convert a service-manager probe into a doctor finding.
fn service_finding(facts: &PumpFacts) -> Finding {
    match facts.service {
        ServicePresence::Installed => Finding::ok(SECTION, SERVICE, "service installed"),
        ServicePresence::Missing => Finding::warn(
            SECTION,
            SERVICE,
            "pump service not installed",
            "vivi pump service install",
        ),
        ServicePresence::Unavailable => {
            Finding::skipped(SECTION, SERVICE, "service manager unavailable")
        }
    }
}

/// Return the age of `<home>/status.txt` in whole seconds, if readable.
fn tick_age_secs(home: &Path) -> Option<u64> {
    let modified =
        match std::fs::metadata(home.join("status.txt")).and_then(|metadata| metadata.modified()) {
            Ok(modified) => modified,
            Err(_) => return None,
        };
    Some(
        SystemTime::now()
            .duration_since(modified)
            .map_or(0, |elapsed| elapsed.as_secs()),
    )
}

/// Probe the platform service manager using an injectable command runner.
fn probe_service() -> ServicePresence {
    probe_service_with(|program, args| {
        let output = std::process::Command::new(program).args(args).output()?;
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    })
}

/// Interpret launchd/systemd results; injected runners keep tests shell-free.
fn probe_service_with(
    mut run: impl FnMut(&str, &[&str]) -> std::io::Result<(bool, String)>,
) -> ServicePresence {
    #[cfg(target_os = "macos")]
    {
        let uid_text = match run("id", &["-u"]) {
            Ok((true, text)) => text,
            _ => return ServicePresence::Unavailable,
        };
        let uid = uid_text.trim();
        if uid.is_empty() {
            return ServicePresence::Unavailable;
        }
        let target = format!("gui/{uid}/{SERVICE_LABEL}");
        return match run("launchctl", &["print", &target]) {
            Ok((true, _)) => ServicePresence::Installed,
            Ok((false, _)) => ServicePresence::Missing,
            Err(_) => ServicePresence::Unavailable,
        };
    }
    #[cfg(target_os = "linux")]
    {
        return match run("systemctl", &["--user", "is-active", SERVICE_UNIT]) {
            Ok((true, state)) if state.trim() == "active" => ServicePresence::Installed,
            Ok(_) => ServicePresence::Missing,
            Err(_) => ServicePresence::Unavailable,
        };
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        drop(run);
        ServicePresence::Unavailable
    }
}

#[cfg(test)]
#[path = "pump_section_test.rs"]
mod tests;
