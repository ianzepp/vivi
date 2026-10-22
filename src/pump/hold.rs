//! The not-ready hold: a pump on a project that is missing required config.
//!
//! The pump takes its lock, says what is missing in `status.txt` and
//! `log.txt`, and dispatches nothing. A long-running pump keeps watching the
//! config and starts for real the moment the last item is supplied; `pump
//! once` reports and exits cleanly. Neither exits with an error, so the
//! service supervisor, which restarts a pump that dies, never loops on it.

use std::fs::{self, File};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::VivariumError;
use crate::mailspace::Mailspace;
use crate::pump::control::{self, PumpControl, PumpMode};
use crate::pump::host::{Host, RealHost};
use crate::pump::readiness;
use crate::routing::{self, Issue, Severity};

/// How a held pump was started.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HoldMode {
    /// `pump once`: report and return instead of waiting.
    pub once: bool,
    /// A service child announces readiness to its supervisor.
    pub announce_ready: bool,
}

/// Hold the pump while `issues` still list missing items.
///
/// `lock` is the pump lock the caller took; it is released just before the
/// pump restarts for real, and held until then.
///
/// # Errors
/// Returns whatever a restart on a now-ready project fails with.
pub(crate) fn hold(
    project: &Path,
    home: &Path,
    tick: u64,
    issues: Vec<Issue>,
    control: PumpControl,
    mode: HoldMode,
    lock: File,
) -> Result<(), VivariumError> {
    let host = RealHost::new(home);
    if mode.announce_ready {
        println!("VIVI_PUMP_SERVICE_READY");
        std::io::Write::flush(&mut std::io::stdout())?;
    }
    if control.mode == PumpMode::Stopped {
        host.log("pump is stopped; run vivi pump start to enable it");
        return Ok(());
    }
    let mut issues = issues;
    let mut tick = tick;
    let mut logged = None;
    loop {
        report(home, &host, &issues, &mut logged);
        if mode.once || wait_for_recheck(home, tick) {
            return Ok(());
        }
        match reassess(project) {
            Ok((next, _)) if readiness::is_ready(&next) => {
                host.log("pump is ready; starting");
                drop(lock);
                return crate::pump::run::run_inner(project, false, false);
            }
            Ok((next, next_tick)) => {
                issues = next;
                tick = next_tick;
            }
            Err(error) => report_unreadable(&host, &error, &mut logged),
        }
    }
}

/// The current findings and the pump's tick, from a fresh read of the config.
fn reassess(project: &Path) -> Result<(Vec<Issue>, u64), VivariumError> {
    let mailspace = Mailspace::discover(Some(project))?;
    let Some(cfg) = mailspace.config.pump.as_ref() else {
        return Err(VivariumError::Config(String::from(
            "no [pump] table in .vivi/mailspace.toml",
        )));
    };
    Ok((readiness::assess(&mailspace), cfg.tick.max(1)))
}

/// Write the hold into `status.txt` every pass and into `log.txt` when it
/// changes.
fn report(home: &Path, host: &RealHost, issues: &[Issue], logged: &mut Option<String>) {
    let mut lines = vec![crate::pump::run::utc_stamp(host.now())];
    let missing = readiness::render_missing(issues);
    if !missing.is_empty() {
        lines.push(missing);
    }
    let errors: Vec<&Issue> = issues
        .iter()
        .filter(|issue| issue.severity == Severity::Error)
        .collect();
    for issue in &errors {
        lines.push(format!("invalid: {}", issue.message));
    }
    lines.push(String::from("pump is holding: nothing is dispatched"));
    let path = home.join("status.txt");
    if let Err(error) = fs::write(&path, lines.join("\n") + "\n") {
        host.log(&format!(
            "status write failed at {}: {error}",
            path.display()
        ));
    }
    let summary = routing::not_ready_summary(issues)
        .or_else(|| (!errors.is_empty()).then(|| String::from("config invalid")))
        .unwrap_or_default();
    if logged.as_deref() != Some(summary.as_str()) {
        host.log(&format!("pump holding: {summary}"));
        *logged = Some(summary);
    }
}

fn report_unreadable(host: &RealHost, error: &VivariumError, logged: &mut Option<String>) {
    let line = format!("config unreadable: {error}");
    if logged.as_deref() != Some(line.as_str()) {
        host.log(&format!("pump holding: {line}"));
        *logged = Some(line);
    }
}

/// Sleep one tick in one-second slices, returning `true` when control asked
/// the pump to exit (stopped, or a shutdown request).
fn wait_for_recheck(home: &Path, tick: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(tick);
    loop {
        if exit_requested(home) {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        thread::sleep((deadline - now).min(Duration::from_secs(1)));
    }
}

fn exit_requested(home: &Path) -> bool {
    control::read(home).map_or(true, |state| {
        state.mode == PumpMode::Stopped || state.shutdown_requested
    })
}

#[cfg(test)]
#[path = "hold_test.rs"]
mod tests;
