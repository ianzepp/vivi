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
fn a_pump_merge_role_without_a_pool_is_not_ready_not_failed() {
    let views = [role("merge", Some("merge"), None, None)];
    let finding = merge_pool_finding(&views, &[]);
    assert_eq!(finding.status, Status::Skipped, "{finding:#?}");
    assert!(finding.detail.starts_with("not ready: "), "{finding:#?}");
    assert!(finding.detail.contains("no pool"), "{finding:#?}");
    assert!(
        finding
            .detail
            .contains("vivi role pool merge <pool> --project <root>"),
        "{finding:#?}"
    );
}

#[test]
fn a_pump_merge_role_naming_an_unknown_pool_still_fails_doctor() {
    let mut views = [role("merge", Some("merge"), None, None)];
    views[0].pool = Some(String::from("nowhere"));
    let finding = merge_pool_finding(&views, &["workers"]);
    assert_eq!(finding.status, Status::Fail, "{finding:#?}");
    assert!(finding.detail.contains("unknown pool"), "{finding:#?}");
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
fn a_missing_merge_role_is_not_ready_when_the_pump_is_configured() {
    let views = [role("hand", Some("hand"), None, None)];
    let finding = merge_pool_finding(&views, &["workers"]);
    assert_eq!(finding.status, Status::Skipped, "{finding:#?}");
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
            "pump role researcher",
            "pump role admitter",
            "pump role slicer",
            "pump role merge kind",
            "pump role auditor",
            "pump role hand kind"
        ]
    );
    // Only the merge role has a pool here, and that pool has no harness: a
    // missing item, so doctor reports it as not ready rather than failing.
    let failing: Vec<&str> = models
        .iter()
        .filter(|f| f.status == Status::Fail)
        .map(|f| f.name.as_str())
        .collect();
    assert!(failing.is_empty(), "{failing:?}");
    assert_eq!(models[4].status, Status::Skipped, "{models:#?}");
    assert!(
        models[4].detail.contains("[pools.here.harness]"),
        "{models:#?}"
    );
    assert!(
        models[4]
            .detail
            .contains("vivi pool bands set <pool> <band> <model> <reasoning> --project <root>"),
        "{models:#?}"
    );
    // Wrong kinds fail; an absent flow role is only a warning (labelled as
    // one); other absent roles and pools are not ready.
    for finding in &findings {
        if finding.name.ends_with(" kind") {
            assert_eq!(finding.status, Status::Fail, "{finding:#?}");
            assert_eq!(
                finding.fix.as_deref(),
                Some("vivi config migrate --project <root>")
            );
        } else if ["researcher", "admitter", "slicer"]
            .iter()
            .any(|role| finding.name == format!("pump role {role}"))
        {
            assert_eq!(finding.status, Status::Warn, "{finding:#?}");
            assert!(!finding.detail.starts_with("not ready"), "{finding:#?}");
        } else {
            assert_eq!(finding.status, Status::Skipped, "{finding:#?}");
            assert!(finding.detail.starts_with("not ready: "), "{finding:#?}");
        }
    }
    assert!(findings[0].detail.contains("pump-planner"));
    assert!(findings[5].detail.contains("must name a pool"));
    assert!(findings[6].detail.contains("not authorized to publish"));
}

#[test]
fn doctor_passes_a_pump_with_every_required_role() {
    let config = pump_config(
        "[[identities]]\nname = \"hand\"\nkind = \"hand\"\npool = \"here\"\n[[identities]]\nname = \"merge\"\nkind = \"merge\"\npool = \"here\"\nmodel = \"P2\"\n[[identities]]\nname = \"pump-planner\"\nkind = \"planner\"\npool = \"here\"\nmodel = \"P2\"\n[[identities]]\nname = \"auditor\"\nkind = \"auditor\"\npool = \"here\"\nmodel = \"P2\"\n[[identities]]\nname = \"researcher\"\nkind = \"researcher\"\npool = \"here\"\nmodel = \"P2\"\n[[identities]]\nname = \"admitter\"\nkind = \"admitter\"\npool = \"here\"\nmodel = \"P2\"\n[[identities]]\nname = \"slicer\"\nkind = \"slicer\"\npool = \"here\"\nmodel = \"P2\"\n[pools.here.harness]\nname = \"fake\"\nrun = \"fake --model {model} @{brief}\"\nmodels = [\"m/ok\"]\n",
    );
    assert!(
        pump_role_findings(&config)
            .iter()
            .all(|f| f.status == Status::Ok)
    );
    let no_pump: crate::mailspace::MailspaceConfig = toml::from_str("name = \"t\"\n").unwrap();
    assert!(pump_role_findings(&no_pump).is_empty());
}

#[test]
fn doctor_labels_a_warning_finding_as_a_warning_not_as_not_ready() {
    // The admitter has no pool: a warning, so it is not "not ready" and not a
    // failure.
    let config = pump_config("[[identities]]\nname = \"admitter\"\nkind = \"admitter\"\n");
    let findings = pump_role_findings(&config);
    let admitter = findings
        .iter()
        .find(|f| f.name == "pump role admitter")
        .expect("admitter finding");
    assert_eq!(admitter.status, Status::Warn, "{admitter:#?}");
    assert!(!admitter.detail.starts_with("not ready"), "{admitter:#?}");
    assert!(
        admitter.detail.contains("must name a pool"),
        "{admitter:#?}"
    );
    // A flow role that is absent entirely is also only a warning.
    let slicer = findings
        .iter()
        .find(|f| f.name == "pump role slicer")
        .expect("slicer finding");
    assert_eq!(slicer.status, Status::Warn, "{slicer:#?}");
    // The pump-planner is absent: that one really is not ready.
    let planner = findings
        .iter()
        .find(|f| f.name == "pump role pump-planner")
        .expect("planner finding");
    assert_eq!(planner.status, Status::Skipped, "{planner:#?}");
    assert!(planner.detail.starts_with("not ready: "), "{planner:#?}");
}
