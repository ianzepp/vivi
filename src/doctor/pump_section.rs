//! The pump section of `vivi doctor`: control mode, lock, tick age, and service.
//!
//! Checks are pure functions over [`PumpFacts`]. The gatherer alone reads the
//! filesystem and probes the service manager.

use std::path::Path;
use std::time::SystemTime;

use super::{Finding, Project};
use crate::pump::config::PumpConfig;
use crate::pump::control::PumpMode;
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

/// What the gatherer learned about one project's pump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PumpFacts {
    pub home_exists: bool,
    pub mode: Option<PumpMode>,
    pub shutdown_requested: Option<bool>,
    pub lock_held: Option<bool>,
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

/// Store-backed diagnostics that are safe to pass through the pure renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PumpStateFacts {
    pub exceptions: Vec<String>,
    pub quarantines: Option<usize>,
}

/// Gather pump facts through the read-only project context.
#[must_use]
pub fn findings(project: &Project) -> Vec<Finding> {
    let Some(config) = project.config.pump.as_ref() else {
        return vec![Finding::skipped(SECTION, HOME, "pump not configured")];
    };
    let home = config.home_dir(&project.root);
    let exists = match home.try_exists() {
        Ok(exists) => exists,
        Err(_) => {
            return vec![Finding::skipped(
                SECTION,
                HOME,
                "pump home cannot be inspected",
            )];
        }
    };
    if !exists {
        return findings_from(&missing_home_facts());
    }
    let mut findings = findings_from(&gather_facts(&home, probe_service));
    findings.extend(pump_state_findings(config, &project.root, &home));
    findings
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
    findings.push(lock_finding(facts));
    findings.push(tick_finding(facts));
    findings.push(service_finding(facts));
    findings
}

/// Render store-backed exception and quarantine findings.
#[must_use]
pub fn state_findings_from(facts: &PumpStateFacts) -> Vec<Finding> {
    let mut out = vec![exceptions_finding(&facts.exceptions)];
    out.push(match facts.quarantines {
        Some(0) => Finding::ok(SECTION, "quarantines", "no quarantines"),
        Some(count) => Finding::warn(
            SECTION,
            "quarantines",
            &format!("{count} quarantine branch(es) pending repair"),
            "the pump mints repair units automatically; see vivi pump log --exceptions",
        ),
        None => Finding::skipped(SECTION, "quarantines", "integration clone unavailable"),
    });
    out
}

fn pump_state_findings(config: &PumpConfig, project: &Path, home: &Path) -> Vec<Finding> {
    let store = match PumpStore::open_read_only(&home.join(DB_FILENAME)) {
        Ok(Some(store)) => store,
        Ok(None) => return vec![Finding::skipped(SECTION, "state", "no pump state yet")],
        Err(_) => {
            return vec![Finding::fail(
                SECTION,
                "state",
                "pump state unreadable",
                "inspect .vivi/pump/pump.db",
            )];
        }
    };
    let exceptions = match store.exceptions() {
        Ok(rows) => rows
            .iter()
            .map(|(_, kind, unit, _)| format!("{kind} ({unit})"))
            .collect(),
        Err(_) => {
            return vec![Finding::fail(
                SECTION,
                "exceptions",
                "pump exceptions unreadable",
                "inspect .vivi/pump/pump.db",
            )];
        }
    };
    let quarantines = quarantine_count(config, project);
    state_findings_from(&PumpStateFacts {
        exceptions,
        quarantines,
    })
}

fn exceptions_finding(exceptions: &[String]) -> Finding {
    if exceptions.is_empty() {
        Finding::ok(SECTION, "exceptions", "no open exceptions")
    } else {
        let shown = exceptions
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        Finding::warn(
            SECTION,
            "exceptions",
            &format!("{} open exception(s): {shown}", exceptions.len()),
            "vivi pump log --exceptions --project <root>, then requeue or repair",
        )
    }
}

fn quarantine_count(config: &PumpConfig, project: &Path) -> Option<usize> {
    let targets = config.target_configs();
    let mut total = 0_usize;
    for target in targets.keys() {
        let repo = config.target_repo_dir(project, target)?;
        total = total.checked_add(count_quarantines(&repo)?)?;
    }
    Some(total)
}

fn count_quarantines(repo: &Path) -> Option<usize> {
    let prefix = format!(
        "refs/heads/{}",
        crate::pump::need::quarantine_namespace_prefix()
    );
    let output = match std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["for-each-ref", "--format=%(refname)"])
        .arg(prefix)
        .output()
    {
        Ok(output) => output,
        Err(_) => return None,
    };
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .count(),
    )
}

fn missing_home_facts() -> PumpFacts {
    PumpFacts {
        home_exists: false,
        mode: None,
        shutdown_requested: None,
        lock_held: None,
        tick_age_secs: None,
        service: ServicePresence::Unavailable,
    }
}

/// Probe the live pump. The service callback is injectable so callers and
/// tests can gather local facts without invoking a system command.
fn gather_facts(home: &Path, service_probe: impl FnOnce() -> ServicePresence) -> PumpFacts {
    let control = match crate::pump::control::inspect(home) {
        Ok(control) => Some(control),
        Err(_) => None,
    };
    let (mode, shutdown_requested) = control.map_or((None, None), |control| {
        (Some(control.mode), Some(control.shutdown_requested))
    });
    PumpFacts {
        home_exists: true,
        mode,
        shutdown_requested,
        lock_held: match crate::pump::run::lock_is_held(home) {
            Ok(held) => Some(held),
            Err(_) => None,
        },
        tick_age_secs: tick_age_secs(home),
        service: service_probe(),
    }
}

/// Report the operator control mode and a requested shutdown.
fn control_finding(facts: &PumpFacts) -> Finding {
    match (facts.mode, facts.shutdown_requested) {
        (None, _) | (_, None) => {
            Finding::skipped(SECTION, CONTROL, "pump control state unreadable")
        }
        (Some(PumpMode::Paused), _) => Finding::warn(
            SECTION,
            CONTROL,
            "pump is paused",
            "vivi pump resume --project <root>",
        ),
        (Some(PumpMode::Stopped), _) => {
            Finding::warn(SECTION, CONTROL, "pump is stopped", START_FIX)
        }
        (Some(PumpMode::Running), Some(true)) => {
            Finding::warn(SECTION, CONTROL, "shutdown requested", START_FIX)
        }
        (Some(PumpMode::Running), Some(false)) => {
            Finding::ok(SECTION, CONTROL, "control mode running")
        }
    }
}

/// Report whether a pump process owns its lock.
fn lock_finding(facts: &PumpFacts) -> Finding {
    match facts.lock_held {
        None => Finding::skipped(SECTION, LOCK, "pump lock status unavailable"),
        Some(false) => Finding::warn(
            SECTION,
            LOCK,
            "no pump process holds the lock",
            "vivi pump run --project <root> (or vivi pump service install)",
        ),
        Some(true) if facts.mode == Some(PumpMode::Running) => {
            Finding::ok(SECTION, LOCK, "pump running")
        }
        Some(true) => {
            Finding::skipped(SECTION, LOCK, "pump lock held while control is not running")
        }
    }
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
