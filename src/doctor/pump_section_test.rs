use super::*;
use crate::doctor::Status;
use crate::pump::control::PumpMode;

/// Healthy pump facts: everything ok except the named field.
fn facts(home_exists: bool, mode: PumpMode) -> PumpFacts {
    PumpFacts {
        home_exists,
        mode,
        shutdown_requested: false,
        lock_held: true,
        tick_age_secs: Some(60),
        service: ServicePresence::Installed,
    }
}

fn finding(findings: &[Finding], name: &str) -> Finding {
    findings
        .iter()
        .find(|finding| finding.name == name)
        .cloned()
        .unwrap_or_else(|| panic!("no {name} finding in {findings:#?}"))
}

#[test]
fn missing_home_skips() {
    let findings = findings_from(&facts(false, PumpMode::Running));
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].section, "pump");
    assert_eq!(findings[0].name, "pump home");
    assert_eq!(findings[0].status, Status::Skipped, "{findings:#?}");
    assert_eq!(findings[0].detail, "pump not initialized for this project");
}

#[test]
fn paused_and_stopped_warn_with_fix() {
    let paused = finding(&findings_from(&facts(true, PumpMode::Paused)), "control");
    assert_eq!(paused.status, Status::Warn, "{paused:#?}");
    assert_eq!(
        paused.fix.as_deref(),
        Some("vivi pump resume --project <root>")
    );

    let stopped = finding(&findings_from(&facts(true, PumpMode::Stopped)), "control");
    assert_eq!(stopped.status, Status::Warn, "{stopped:#?}");
    assert_eq!(
        stopped.fix.as_deref(),
        Some("vivi pump start --project <root>")
    );

    let mut shutdown = facts(true, PumpMode::Running);
    shutdown.shutdown_requested = true;
    let shutdown = finding(&findings_from(&shutdown), "control");
    assert_eq!(shutdown.status, Status::Warn, "{shutdown:#?}");
    assert_eq!(shutdown.detail, "shutdown requested");
    assert_eq!(shutdown.fix.as_deref(), Some("vivi pump start --project <root>"));

    let running = finding(&findings_from(&facts(true, PumpMode::Running)), "control");
    assert_eq!(running.status, Status::Ok, "{running:#?}");
}

#[test]
fn stale_tick_warns_and_fresh_ok() {
    let mut never = facts(true, PumpMode::Running);
    never.tick_age_secs = None;
    let never_finding = finding(&findings_from(&never), "tick");
    assert_eq!(never_finding.status, Status::Warn, "{never_finding:#?}");
    assert_eq!(
        never_finding.detail,
        "status.txt missing: the pump has never ticked"
    );

    let mut stale = facts(true, PumpMode::Running);
    stale.tick_age_secs = Some(TICK_STALE_SECS + 1);
    let stale_finding = finding(&findings_from(&stale), "tick");
    assert_eq!(stale_finding.status, Status::Warn, "{stale_finding:#?}");
    assert_eq!(stale_finding.detail, "last tick 7201s ago");
    assert_eq!(
        stale_finding.fix.as_deref(),
        Some("check the pump process and vivi pump log")
    );

    let fresh_finding = finding(&findings_from(&facts(true, PumpMode::Running)), "tick");
    assert_eq!(fresh_finding.status, Status::Ok, "{fresh_finding:#?}");
    assert_eq!(fresh_finding.detail, "last tick 60s ago");
    assert_eq!(fresh_finding.fix, None);
}

#[test]
fn no_lock_warns() {
    let mut unlocked = facts(true, PumpMode::Running);
    unlocked.lock_held = false;
    let lock = finding(&findings_from(&unlocked), "lock");
    assert_eq!(lock.status, Status::Warn, "{lock:#?}");
    assert_eq!(lock.detail, "no pump process holds the lock");
    assert_eq!(
        lock.fix.as_deref(),
        Some("vivi pump run --project <root> (or vivi pump service install)")
    );

    let held = finding(&findings_from(&facts(true, PumpMode::Running)), "lock");
    assert_eq!(held.status, Status::Ok, "{held:#?}");
    assert_eq!(held.detail, "pump running");

    // A paused pump may still hold its lock; the control finding already
    // reports the mode, so the lock finding stays out of the way.
    let paused = findings_from(&facts(true, PumpMode::Paused));
    assert!(
        !paused.iter().any(|finding| finding.name == "lock"),
        "{paused:#?}"
    );
}

#[test]
fn service_missing_warns() {
    let mut missing = facts(true, PumpMode::Running);
    missing.service = ServicePresence::Missing;
    let missing_finding = finding(&findings_from(&missing), "service");
    assert_eq!(missing_finding.status, Status::Warn, "{missing_finding:#?}");
    assert_eq!(missing_finding.detail, "pump service not installed");
    assert_eq!(
        missing_finding.fix.as_deref(),
        Some("vivi pump service install")
    );

    let mut unavailable = facts(true, PumpMode::Running);
    unavailable.service = ServicePresence::Unavailable;
    let unavailable_finding = finding(&findings_from(&unavailable), "service");
    assert_eq!(unavailable_finding.status, Status::Skipped, "{unavailable_finding:#?}");
    assert_eq!(unavailable_finding.detail, "service manager unavailable");

    let installed = finding(&findings_from(&facts(true, PumpMode::Running)), "service");
    assert_eq!(installed.status, Status::Ok, "{installed:#?}");
    assert_eq!(installed.detail, "service installed");
}
