use super::*;
use crate::doctor::Status;

fn facts(home_exists: bool, mode: PumpMode) -> PumpFacts {
    PumpFacts {
        home_exists,
        mode: Some(mode),
        shutdown_requested: Some(false),
        lock_held: Some(true),
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
    assert_eq!(findings[0].status, Status::Skipped);
    assert_eq!(findings[0].detail, "pump not initialized for this project");
}

#[test]
fn paused_and_stopped_warn_with_fix() {
    let paused = finding(&findings_from(&facts(true, PumpMode::Paused)), "control");
    assert_eq!(paused.status, Status::Warn);
    assert_eq!(
        paused.fix.as_deref(),
        Some("vivi pump resume --project <root>")
    );

    let stopped = finding(&findings_from(&facts(true, PumpMode::Stopped)), "control");
    assert_eq!(stopped.status, Status::Warn);
    assert_eq!(stopped.fix.as_deref(), Some(START_FIX));

    let running = finding(&findings_from(&facts(true, PumpMode::Running)), "control");
    assert_eq!(running.status, Status::Ok);

    let mut shutdown = facts(true, PumpMode::Running);
    shutdown.shutdown_requested = Some(true);
    let shutdown = finding(&findings_from(&shutdown), "control");
    assert_eq!(shutdown.detail, "shutdown requested");
    assert_eq!(shutdown.fix.as_deref(), Some(START_FIX));
}

#[test]
fn stale_tick_warns_and_fresh_ok() {
    let mut missing = facts(true, PumpMode::Running);
    missing.tick_age_secs = None;
    let missing = finding(&findings_from(&missing), "tick");
    assert_eq!(missing.status, Status::Warn);
    assert_eq!(
        missing.detail,
        "status.txt missing: the pump has never ticked"
    );

    let mut stale = facts(true, PumpMode::Running);
    stale.tick_age_secs = Some(TICK_STALE_SECS + 1);
    let stale = finding(&findings_from(&stale), "tick");
    assert_eq!(stale.status, Status::Warn);
    assert_eq!(stale.detail, "last tick 7201s ago");
    assert_eq!(stale.fix.as_deref(), Some(LOG_FIX));

    let fresh = finding(&findings_from(&facts(true, PumpMode::Running)), "tick");
    assert_eq!(fresh.status, Status::Ok);
    assert_eq!(fresh.detail, "last tick 60s ago");
    assert_eq!(fresh.fix, None);
}

#[test]
fn no_lock_warns() {
    let mut unlocked = facts(true, PumpMode::Running);
    unlocked.lock_held = Some(false);
    let lock = finding(&findings_from(&unlocked), "lock");
    assert_eq!(lock.status, Status::Warn);
    assert_eq!(lock.detail, "no pump process holds the lock");
    assert_eq!(
        lock.fix.as_deref(),
        Some("vivi pump run --project <root> (or vivi pump service install)")
    );
}

#[test]
fn service_missing_warns() {
    let mut missing = facts(true, PumpMode::Running);
    missing.service = ServicePresence::Missing;
    let missing_finding = finding(&findings_from(&missing), "service");
    assert_eq!(missing_finding.status, Status::Warn);
    assert_eq!(missing_finding.detail, "pump service not installed");
    assert_eq!(
        missing_finding.fix.as_deref(),
        Some("vivi pump service install")
    );

    missing.service = ServicePresence::Unavailable;
    let unavailable = finding(&findings_from(&missing), "service");
    assert_eq!(unavailable.status, Status::Skipped);
    assert_eq!(unavailable.detail, "service manager unavailable");
}

#[test]
fn service_probe_uses_injected_runner() {
    let presence = probe_service_with(|program, _| {
        if cfg!(target_os = "macos") && program == "id" {
            Ok((true, String::from("501\n")))
        } else {
            Ok((false, String::new()))
        }
    });
    #[cfg(target_os = "macos")]
    assert_eq!(presence, ServicePresence::Missing);
    #[cfg(target_os = "linux")]
    assert_eq!(presence, ServicePresence::Missing);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    assert_eq!(presence, ServicePresence::Unavailable);
}

#[test]
fn exceptions_and_quarantines_warn_without_echoing_exception_text() {
    let facts = PumpStateFacts {
        exceptions: vec!["merge-error (u-123)".into()],
        quarantines: Some(2),
        gates: Vec::new(),
    };
    let report = state_findings_from(&facts, 100_000);
    let exceptions = finding(&report, "exceptions");
    assert_eq!(exceptions.status, Status::Warn);
    assert!(exceptions.detail.contains("merge-error (u-123)"));
    assert_eq!(finding(&report, "quarantines").status, Status::Warn);
    assert_eq!(finding(&report, "gate").status, Status::Skipped);
}

#[test]
fn stale_running_gate_warns_and_failed_gate_names_tests() {
    let stale = PumpStateFacts {
        exceptions: Vec::new(),
        quarantines: Some(0),
        gates: vec![GateFact {
            target: "app".into(),
            outcome: GateOutcome::Running { started_at: 1 },
        }],
    };
    let report = state_findings_from(&stale, 10_000);
    assert_eq!(finding(&report, "gate app").status, Status::Warn);

    let failed = PumpStateFacts {
        exceptions: Vec::new(),
        quarantines: Some(0),
        gates: vec![GateFact {
            target: "default".into(),
            outcome: GateOutcome::Failed("check-a".into()),
        }],
    };
    let report = state_findings_from(&failed, 10_000);
    let gate = finding(&report, "gate default");
    assert_eq!(gate.status, Status::Warn);
    assert!(gate.detail.contains("check-a"));
}
