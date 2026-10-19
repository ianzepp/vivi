use super::*;
use crate::doctor::Status;
use crate::pump::state::GateRecord;
use crate::pump::store::append_exception;
use std::fs;

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

fn pump_state_facts(last_gate: Option<String>) -> PumpStateFacts {
    PumpStateFacts {
        open_exceptions: Vec::new(),
        quarantines: 0,
        last_gate,
    }
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
    shutdown.shutdown_requested = true;
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
    unlocked.lock_held = false;
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
fn exceptions_warn_with_log_fix() {
    let mut facts = pump_state_facts(None);
    facts.open_exceptions = vec!["gate-environment: check failed".to_string()];
    let warn = finding(&state_findings_from(&facts, 0), EXCEPTIONS);
    assert_eq!(warn.status, Status::Warn);
    assert_eq!(warn.name, EXCEPTIONS);
    assert_eq!(warn.detail, "1 exception(s): gate-environment: check failed");
    assert_eq!(warn.fix.as_deref(), Some(EXCEPTIONS_FIX));

    facts.open_exceptions = vec![format!("kind: {}", "x".repeat(200))];
    let long = finding(&state_findings_from(&facts, 0), EXCEPTIONS);
    let prefix = "1 exception(s): ";
    assert!(long.detail.starts_with(prefix), "{}", long.detail);
    assert!(long.detail.ends_with("..."), "{}", long.detail);
    assert_eq!(
        long.detail.chars().count(),
        prefix.len() + MAX_SUMMARY_CHARS + 3
    );

    let ok = finding(&state_findings_from(&pump_state_facts(None), 0), EXCEPTIONS);
    assert_eq!(ok.status, Status::Ok);
    assert_eq!(ok.detail, "no open exceptions");
}

#[test]
fn quarantines_warn() {
    let mut facts = pump_state_facts(None);
    facts.quarantines = 2;
    let warn = finding(&state_findings_from(&facts, 0), QUARANTINE);
    assert_eq!(warn.status, Status::Warn);
    assert_eq!(warn.name, QUARANTINE);
    assert_eq!(warn.detail, "2 quarantine branch(es) pending repair");
    assert_eq!(warn.fix.as_deref(), Some(QUARANTINE_FIX));

    let ok = finding(
        &state_findings_from(&pump_state_facts(None), 0),
        QUARANTINE,
    );
    assert_eq!(ok.status, Status::Ok);
    assert_eq!(ok.detail, "no quarantines");
}

#[test]
fn stale_running_gate_warns() {
    let facts = pump_state_facts(Some("running since 1000".to_string()));
    let stuck = finding(
        &state_findings_from(&facts, 1000 + GATE_STALE_SECS + 1),
        GATE,
    );
    assert_eq!(stuck.status, Status::Warn);
    assert_eq!(stuck.name, GATE);
    assert_eq!(stuck.detail, "gate stuck");
    assert_eq!(stuck.fix.as_deref(), Some(GATE_STUCK_FIX));

    let fresh = finding(&state_findings_from(&facts, 1000 + 60), GATE);
    assert_eq!(fresh.status, Status::Ok);
    assert_eq!(fresh.detail, "running since 1000");
}

#[test]
fn no_gate_skips() {
    let no_gate = finding(&state_findings_from(&pump_state_facts(None), 0), GATE);
    assert_eq!(no_gate.status, Status::Skipped);
    assert_eq!(no_gate.name, GATE);
    assert_eq!(no_gate.detail, "no gate has run");
}

#[test]
fn failed_gate_warns_passed_ok() {
    let mut state = State::default();
    state.last_gate = GateRun {
        at: 1_000,
        ..GateRun::default()
    };
    state.gates.push(GateRecord {
        at: 2_000,
        names: Some(vec!["doctor::a".to_string(), "doctor::b".to_string()]),
        ..GateRecord::default()
    });
    let failing = finding(
        &state_findings_from(&pump_state_facts(last_gate_label(&state)), 2_100),
        GATE,
    );
    assert_eq!(failing.status, Status::Warn);
    assert_eq!(failing.name, GATE);
    assert_eq!(
        failing.detail,
        "last gate failed at 2000: doctor::a, doctor::b"
    );
    assert_eq!(failing.fix.as_deref(), Some(GATE_FAILED_FIX));

    state.gates[0].names = Some(Vec::new());
    let passed = finding(
        &state_findings_from(&pump_state_facts(last_gate_label(&state)), 2_100),
        GATE,
    );
    assert_eq!(passed.status, Status::Ok);
    assert_eq!(passed.detail, "last gate passed at 2000");

    state.gates[0].names = None;
    let unreadable = finding(
        &state_findings_from(&pump_state_facts(last_gate_label(&state)), 2_100),
        GATE,
    );
    assert_eq!(unreadable.status, Status::Warn);
    assert_eq!(unreadable.detail, "last gate failed at 2000");
}

#[test]
fn gather_reads_real_store() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _space = Mailspace::init(Some(tmp.path())).expect("init");
    let home = tmp.path().join(".vivi").join("pump");
    fs::create_dir_all(&home).expect("pump home");
    let mut store = PumpStore::open(&home.join(DB_FILENAME)).expect("open store");
    let state = State {
        last_gate: GateRun {
            at: 1_000,
            task: Some("task-1".to_string()),
            ..GateRun::default()
        },
        ..State::default()
    };
    store.save(&state).expect("save state");
    drop(store);
    append_exception(&home, 10, "gate-environment", "u1", "check failed");

    let facts = state_facts(tmp.path(), &PumpConfig::default()).expect("facts");
    assert_eq!(
        facts.open_exceptions,
        vec!["gate-environment: check failed".to_string()]
    );
    assert_eq!(facts.quarantines, 0);
    assert_eq!(facts.last_gate.as_deref(), Some("running since 1000"));

    let warn = finding(
        &store_findings(tmp.path(), &PumpConfig::default()),
        EXCEPTIONS,
    );
    assert_eq!(warn.status, Status::Warn);
    assert_eq!(warn.detail, "1 exception(s): gate-environment: check failed");
    assert_eq!(warn.fix.as_deref(), Some(EXCEPTIONS_FIX));
}
