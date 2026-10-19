//! The pump section of `vivi doctor`: control mode, lock, tick age, and service.
//!
//! Checks are pure functions over [`PumpFacts`]. The gatherer alone reads the
//! filesystem and probes the service manager.

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

/// Gather and report pump facts for a project, mirroring `pump run status`.
#[must_use]
pub fn findings(project: &Path) -> Vec<Finding> {
    let config = match Mailspace::discover(Some(project)) {
        Ok(mailspace) => mailspace.config.pump.unwrap_or_default(),
        Err(_) => PumpConfig::default(),
    };
    let home = config.home_dir(project);
    if !home.exists() {
        return findings_from(&missing_home_facts());
    }
    findings_from(&gather_facts(&home, probe_service))
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

/// Probe the live pump. The service callback is injectable so callers and
/// tests can gather local facts without invoking a system command.
fn gather_facts(
    home: &Path,
    service_probe: impl FnOnce() -> ServicePresence,
) -> PumpFacts {
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
    let modified = match std::fs::metadata(home.join("status.txt"))
        .and_then(|metadata| metadata.modified())
    {
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
        let _ = run;
        ServicePresence::Unavailable
    }
}

#[cfg(test)]
#[path = "pump_section_test.rs"]
mod tests;
