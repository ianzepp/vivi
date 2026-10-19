use super::*;
use crate::doctor::Status;

fn role(name: &str, kind: Option<&str>, pid: Option<u32>, cadence: Option<&str>) -> RoleView {
    RoleView {
        name: name.to_string(),
        address: format!("{name}@local"),
        aliases: Vec::new(),
        kind: kind.map(str::to_string),
        labels: Vec::new(),
        status: "active".to_string(),
        harness: None,
        provider: None,
        model: None,
        thinking: None,
        pid,
        host: None,
        cadence: cadence.map(str::to_string),
        pool: None,
        read_only: None,
        models: Vec::new(),
        charter: String::new(),
        has_charter: false,
    }
}

fn finding<'a>(findings: &'a [Finding], name: &str) -> &'a Finding {
    findings
        .iter()
        .find(|finding| finding.name == name)
        .unwrap_or_else(|| panic!("no {name} finding in {findings:#?}"))
}

#[test]
fn empty_roster_is_reported_without_head_cadence_noise() {
    let findings = findings_from(&[]);
    assert_eq!(finding(&findings, "roster").status, Status::Warn);
    assert_eq!(finding(&findings, "head cadence").status, Status::Skipped);
    assert_eq!(
        finding(&findings, "bindings").detail,
        "0 bound process(es) have no dead or zombie binding"
    );
}

#[test]
fn head_cadence_and_non_head_cadence_are_checked_separately() {
    let views = [
        role("mind", Some("HEAD"), None, None),
        role("auditor", Some("auditor"), None, Some("30m")),
        role("scoper", Some("scoper"), None, Some("15m")),
    ];
    let findings = findings_from(&views);
    let head = finding(&findings, "head cadence");
    assert_eq!(head.status, Status::Warn);
    assert!(head.detail.contains("mind"));

    let non_head = finding(&findings, "non-head cadence");
    assert_eq!(non_head.status, Status::Warn);
    assert!(non_head.detail.contains("auditor"));
    assert!(non_head.detail.contains("scoper"));
}

#[test]
fn cadence_boundaries_match_due_and_overdue_thresholds() {
    assert_eq!(cadence_state(109, 100), ScheduleState::Ok);
    assert_eq!(cadence_state(110, 100), ScheduleState::Due);
    assert_eq!(cadence_state(199, 100), ScheduleState::Due);
    assert_eq!(cadence_state(200, 100), ScheduleState::Overdue);
}

#[test]
fn a_bound_dead_pid_warns_without_disclosing_process_details() {
    let views = [role("worker", Some("hand"), Some(u32::MAX), None)];
    let bindings = finding(&findings_from(&views), "bindings").clone();
    assert_eq!(bindings.status, Status::Warn, "{bindings:#?}");
    assert!(bindings.detail.contains("worker"));
    assert!(!bindings.detail.contains("command"));
}
