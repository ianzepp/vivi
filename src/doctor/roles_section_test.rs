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
fn a_pump_merge_role_without_a_pool_fails_doctor() {
    let views = [role("merge", Some("merge"), None, None)];
    let finding = merge_pool_finding(&views, &[]);
    assert_eq!(finding.status, Status::Fail, "{finding:#?}");
    assert!(finding.detail.contains("no pool"), "{finding:#?}");
    assert_eq!(
        finding.fix.as_deref(),
        Some("vivi role pool merge <pool> --project <root>")
    );
}

#[test]
fn a_bound_merge_pool_is_ok() {
    let mut views = [role("merge", Some("merge"), None, None)];
    views[0].pool = Some(String::from("workers"));
    let finding = merge_pool_finding(&views, &["workers"]);
    assert_eq!(finding.status, Status::Ok, "{finding:#?}");
    assert!(finding.detail.contains("workers"), "{finding:#?}");
}

#[test]
fn a_missing_merge_role_fails_when_the_pump_is_configured() {
    let views = [role("hand", Some("hand"), None, None)];
    let finding = merge_pool_finding(&views, &["workers"]);
    assert_eq!(finding.status, Status::Fail, "{finding:#?}");
    assert!(finding.detail.contains("no merge role"), "{finding:#?}");
}

#[test]
fn a_bound_dead_pid_warns_without_disclosing_process_details() {
    let views = [role("worker", Some("hand"), Some(u32::MAX), None)];
    let bindings = finding(&findings_from(&views), "bindings").clone();
    assert_eq!(bindings.status, Status::Warn, "{bindings:#?}");
    assert!(bindings.detail.contains("worker"));
    assert!(!bindings.detail.contains("command"));
}

fn pump_config(roles: &str) -> crate::mailspace::MailspaceConfig {
    toml::from_str(&format!(
        "name = \"t\"\n{roles}[pools.here]\nprovider = \"local\"\nslots = 1\n[pools.here.bands]\nP2 = [{{ model = \"m/ok\", reasoning = \"high\" }}]\n[pump]\nbranch = \"next\"\nto = \"hand\"\n"
    ))
    .unwrap()
}

#[test]
fn doctor_reports_the_pump_role_requirements_that_validate_enforces() {
    let config = pump_config(
        "[[identities]]\nname = \"hand\"\nkind = \"mind\"\n[[identities]]\nname = \"merge\"\nkind = \"hand\"\npool = \"here\"\n[[identities]]\nname = \"planner\"\nkind = \"planner\"\npool = \"here\"\n[[identities]]\nname = \"auditor\"\nkind = \"auditor\"\n",
    );
    let all = pump_role_findings(&config);
    let (models, findings): (Vec<_>, Vec<_>) =
        all.into_iter().partition(|f| f.name.ends_with(" model"));
    let names: Vec<&str> = findings.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "pump role pump-planner",
            "pump role merge kind",
            "pump role auditor",
            "pump role hand kind"
        ]
    );
    // Only the merge role has a pool here, and that pool has no harness.
    let failing: Vec<&str> = models
        .iter()
        .filter(|f| f.status == Status::Fail)
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(failing, ["pump role merge model"]);
    assert!(
        models[1].detail.contains("[pools.here.harness]"),
        "{models:#?}"
    );
    assert_eq!(
        models[1].fix.as_deref(),
        Some("vivi pool bands set <pool> <band> <model> <reasoning> --project <root>")
    );
    for finding in &findings {
        assert_eq!(finding.status, Status::Fail, "{finding:#?}");
        assert_eq!(
            finding.fix.as_deref(),
            Some("vivi config migrate --project <root>")
        );
    }
    assert!(findings[0].detail.contains("pump-planner"));
    assert!(findings[2].detail.contains("must name a pool"));
    assert!(findings[3].detail.contains("not authorized to publish"));
}

#[test]
fn doctor_passes_a_pump_with_every_required_role() {
    let config = pump_config(
        "[[identities]]\nname = \"hand\"\nkind = \"hand\"\npool = \"here\"\n[[identities]]\nname = \"merge\"\nkind = \"merge\"\npool = \"here\"\nmodel = \"P2\"\n[[identities]]\nname = \"pump-planner\"\nkind = \"planner\"\npool = \"here\"\nmodel = \"P2\"\n[[identities]]\nname = \"auditor\"\nkind = \"auditor\"\npool = \"here\"\nmodel = \"P2\"\n[pools.here.harness]\nname = \"fake\"\nrun = \"fake --model {model} @{brief}\"\nmodels = [\"m/ok\"]\n",
    );
    assert!(
        pump_role_findings(&config)
            .iter()
            .all(|f| f.status == Status::Ok)
    );
    let no_pump: crate::mailspace::MailspaceConfig = toml::from_str("name = \"t\"\n").unwrap();
    assert!(pump_role_findings(&no_pump).is_empty());
}
