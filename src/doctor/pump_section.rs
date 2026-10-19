//! The pump section of `vivi doctor`: control mode, lock, tick age, and the
//! platform service.
//!
//! The checks are pure functions over [`PumpFacts`]; only the gatherer
//! [`findings`] touches the filesystem or shells out, so tests construct
//! facts directly and never probe a service manager.

use std::path::Path;
use std::time::SystemTime;

use super::Finding;
use crate::mailspace::Mailspace;
use crate::pump::config::PumpConfig;
use crate::pump::control::{PumpControl, PumpMode};

const SECTION: &str = "pump";
const HOME: &str = "pump home";
const CONTROL: &str = "control";
const LOCK: &str = "lock";
const TICK: &str = "tick";
const SERVICE: &str = "service";
/// A tick older than this means the pump process is likely wedged.
const TICK_STALE_SECS: u64 = 2 * 3600;
const START_FIX: &str = "vivi pump start --project <root>";
const LOG_FIX: &str = "check the pump process and vivi pump log";

#[cfg(target_os = "macos")]
const SERVICE_LABEL: &str = "com.vivi.pump-service";
#[cfg(target_os = "linux")]
const SERVICE_UNIT: &str = "vivi-pump-service.service";

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

/// The pump section of `vivi doctor`, from what a probe of `project` found.
///
/// Mirrors `pump_run::status`: the configured pump home, falling back to the
/// default home when no mailspace can be discovered.
#[must_use]
pub fn findings(project: &Path) -> Vec<Finding> {
    let config = match Mailspace::discover(Some(project)) {
        Ok(mailspace) => mailspace.config.pump.unwrap_or_default(),
        Err(_) => PumpConfig::default(),
    };
    let home = config.home_dir(project);
    if !home.exists() {
        let facts = PumpFacts {
            home_exists: false,
            mode: PumpMode::Running,
            shutdown_requested: false,
            lock_held: false,
            tick_age_secs: None,
            service: ServicePresence::Missing,
        };
        return findings_from(&facts);
    }
    findings_from(&gather_facts(&home))
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
    findings.extend(lock_finding(facts));
    findings.push(tick_finding(facts));
    findings.push(service_finding(facts));
    findings
}

/// Probe the live pump for one project's home and fold it into facts.
fn gather_facts(home: &Path) -> PumpFacts {
    let control = crate::pump::control::inspect(home)
        .unwrap_or(PumpControl { mode: PumpMode::Running, shutdown_requested: false });
    PumpFacts {
        home_exists: true,
        mode: control.mode,
        shutdown_requested: control.shutdown_requested,
        lock_held: crate::pump::run::lock_is_held(home).unwrap_or(false),
        tick_age_secs: tick_age_secs(home),
        service: probe_service(),
    }
}

/// The operator control mode: paused or stopped warn with their fix; a
/// requested shutdown warns even while the pump is running.
fn control_finding(facts: &PumpFacts) -> Finding {
    match facts.mode {
        PumpMode::Paused => Finding::warn(
            SECTION,
            CONTROL,
            "pump is paused",
            "vivi pump resume --project <root>",
        ),
        PumpMode::Stopped => Finding::warn(SECTION, CONTROL, "pump is stopped", START_FIX),
        PumpMode::Running if facts.shutdown_requested => {
            Finding::warn(SECTION, CONTROL, "shutdown requested", START_FIX)
        }
        PumpMode::Running => Finding::ok(SECTION, CONTROL, &format!("control mode {}", facts.mode)),
    }
}

/// The pump lock: no holder means no pump process is working this project.
/// A held lock only oks while the pump is running; a paused or stopped mode
/// is already reported by the control finding.
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
        Some(Finding::ok(SECTION, LOCK, "pump running"))
    } else {
        None
    }
}

/// How stale `status.txt` is: missing means the pump never ticked; older
/// than [`TICK_STALE_SECS`] means it stopped ticking.
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

/// Whether the pump service is installed on this platform.
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

/// How old `<home>/status.txt` is, in whole seconds, or `None` when it is
/// missing or its modification time cannot be read. Mirrors `status_age`.
fn tick_age_secs(home: &Path) -> Option<u64> {
    let modified = std::fs::metadata(home.join("status.txt"))
        .ok()?
        .modified()
        .ok()?;
    Some(
        SystemTime::now()
            .duration_since(modified)
            .map_or(0, |elapsed| elapsed.as_secs()),
    )
}

/// Probe the platform service manager. The only shell-out in this module;
/// tests construct [`PumpFacts`] directly instead of calling it.
#[cfg(target_os = "macos")]
fn probe_service() -> ServicePresence {
    probe_launchd()
}

/// Probe the platform service manager. The only shell-out in this module;
/// tests construct [`PumpFacts`] directly instead of calling it.
#[cfg(target_os = "linux")]
fn probe_service() -> ServicePresence {
    probe_systemd()
}

/// No supported service manager on this platform.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn probe_service() -> ServicePresence {
    ServicePresence::Unavailable
}

/// `launchctl print gui/<uid>/<label>` success means installed; a nonzero
/// exit or an unknown user means missing; a failed command means the probe
/// could not run.
#[cfg(target_os = "macos")]
fn probe_launchd() -> ServicePresence {
    let Some(uid) = uid() else {
        return ServicePresence::Unavailable;
    };
    let output = std::process::Command::new("launchctl")
        .args(["print", &format!("gui/{uid}/{SERVICE_LABEL}")])
        .output();
    match output {
        Ok(output) if output.status.success() => ServicePresence::Installed,
        Ok(_) => ServicePresence::Missing,
        Err(_) => ServicePresence::Unavailable,
    }
}

#[cfg(target_os = "macos")]
fn uid() -> Option<String> {
    let output = std::process::Command::new("id").arg("-u").output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// `systemctl --user is-active` reporting `active` means installed; any
/// other state means missing; a failed command means the probe could not run.
#[cfg(target_os = "linux")]
fn probe_systemd() -> ServicePresence {
    let output = std::process::Command::new("systemctl")
        .args(["--user", "is-active", SERVICE_UNIT])
        .output();
    match output {
        Ok(output) => {
            let state = String::from_utf8_lossy(&output.stdout).trim();
            if output.status.success() && state == "active" {
                ServicePresence::Installed
            } else {
                ServicePresence::Missing
            }
        }
        Err(_) => ServicePresence::Unavailable,
    }
}

#[cfg(test)]
#[path = "pump_section_test.rs"]
mod tests;
