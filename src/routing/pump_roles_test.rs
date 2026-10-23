use super::*;
use crate::mailspace::MailspaceConfig;
use crate::routing::Severity;

const FLOW_ROLES: [&str; 3] = ["researcher", "admitter", "slicer"];

fn config(roles: &[(&str, &str)]) -> MailspaceConfig {
    let mut text = String::from("name = \"t\"\n");
    for (name, extra) in roles {
        text.push_str(&format!(
            "[[identities]]\nname = \"{name}\"\nkind = \"{name}\"\n{extra}\n"
        ));
    }
    text.push_str(
        "[pools.p]\nprovider = \"local\"\nslots = 2\n[pools.p.local]\nworktree_of = \"/tmp/t\"\n\
         [pools.p.harness]\nname = \"fake\"\nrun = \"fake --model {model} @{brief}\"\n\
         models = [\"m/ok\"]\n\
         [pools.p.bands]\nP2 = [{ model = \"m/ok\", reasoning = \"high\" }]\n\
         [pump]\nbranch = \"next\"\nto = \"hand\"\n",
    );
    toml::from_str(&text).expect("mailspace toml")
}

fn check_of(config: &MailspaceConfig, name: &str) -> PumpRoleCheck {
    pump_role_checks(config)
        .into_iter()
        .find(|check| check.name == name)
        .unwrap_or_else(|| panic!("no check named {name}"))
}

#[test]
fn each_missing_flow_role_is_a_warning() {
    let config = config(&[("hand", "pool = \"p\"")]);
    for role in FLOW_ROLES {
        let check = check_of(&config, role);
        assert_eq!(check.issues.len(), 1, "{role}: {:?}", check.issues);
        let issue = &check.issues[0];
        assert_eq!(issue.severity, Severity::Warning, "{role}");
        assert_eq!(issue.item, None, "{role}");
        assert!(issue.message.contains(role), "{role}");
        assert!(issue.message.contains("vivi config migrate"), "{role}");
        assert!(issue.message.contains("dispatches role steps"), "{role}");
    }
}

#[test]
fn a_flow_role_without_a_pool_is_a_warning() {
    let config = config(&[("researcher", "")]);
    let check = check_of(&config, "researcher");
    assert_eq!(check.issues.len(), 1, "{:?}", check.issues);
    assert_eq!(check.issues[0].severity, Severity::Warning);
    assert!(check.issues[0].message.contains("must name a pool"));
    assert!(check.issues[0].message.contains("dispatches role steps"));
}

#[test]
fn an_inactive_flow_role_is_an_error() {
    let config = config(&[("slicer", "pool = \"p\"\nstatus = \"retired\"")]);
    let check = check_of(&config, "slicer");
    assert!(
        check
            .issues
            .iter()
            .any(|issue| issue.severity == Severity::Error),
        "{:?}",
        check.issues
    );
}

#[test]
fn flow_roles_with_a_pool_meet_the_requirement() {
    let config = config(&[
        ("researcher", "pool = \"p\"\nmodel = \"P2\""),
        ("admitter", "pool = \"p\"\nmodel = \"P2\""),
        ("slicer", "pool = \"p\"\nmodel = \"P2\""),
    ]);
    for role in FLOW_ROLES {
        assert!(check_of(&config, role).issues.is_empty(), "{role}");
        assert!(
            check_of(&config, &format!("{role} model"))
                .issues
                .is_empty(),
            "{role} model"
        );
    }
}

#[test]
fn a_missing_admitter_or_slicer_stays_a_warning() {
    let config = config(&[("hand", "pool = \"p\"")]);
    for role in ["admitter", "slicer"] {
        let issues = check_of(&config, role).issues;
        assert_eq!(issues.len(), 1, "{role}: {issues:?}");
        assert_eq!(issues[0].severity, Severity::Warning, "{role}");
    }
}

#[test]
fn an_admitter_or_slicer_without_a_pool_stays_a_warning() {
    let config = config(&[("admitter", ""), ("slicer", "")]);
    for role in ["admitter", "slicer"] {
        let issues = check_of(&config, role).issues;
        assert_eq!(issues.len(), 1, "{role}: {issues:?}");
        assert_eq!(issues[0].severity, Severity::Warning, "{role}");
        assert!(issues[0].message.contains("must name a pool"), "{role}");
    }
}

fn config_with_kind(name: &str, kind: &str, extra: &str) -> MailspaceConfig {
    let text = format!(
        "name = \"t\"\n[[identities]]\nname = \"{name}\"\nkind = \"{kind}\"\npool = \"p\"\n{extra}\n\
         [pools.p]\nprovider = \"local\"\nslots = 2\n[pump]\nbranch = \"next\"\nto = \"hand\"\n"
    );
    toml::from_str(&text).expect("mailspace toml")
}

#[test]
fn a_slicer_with_kind_hand_is_an_error() {
    let issues = check_of(&config_with_kind("slicer", "hand", ""), "slicer").issues;
    assert!(
        issues
            .iter()
            .any(|issue| issue.severity == Severity::Error && issue.message.contains("kind")),
        "{issues:?}"
    );
}

#[test]
fn a_writable_admitter_or_slicer_is_an_error() {
    for role in ["admitter", "slicer"] {
        let config = config_with_kind(role, role, "read_only = false");
        let issues = check_of(&config, role).issues;
        assert!(
            issues.iter().any(|issue| {
                issue.severity == Severity::Error && issue.message.contains("read-only")
            }),
            "{role}: {issues:?}"
        );
    }
}

#[test]
fn the_researcher_has_no_kind_requirement_yet() {
    let issues = check_of(&config_with_kind("researcher", "hand", ""), "researcher").issues;
    assert!(issues.is_empty(), "{issues:?}");
}

#[test]
fn a_missing_head_cto_or_one_without_a_pool_is_a_warning() {
    let missing = config(&[("hand", "pool = \"p\"")]);
    let check = check_of(&missing, "head-cto");
    assert_eq!(check.issues.len(), 1, "{:?}", check.issues);
    assert_eq!(check.issues[0].severity, Severity::Warning);
    assert!(check.issues[0].message.contains("head-cto"));

    let no_pool = config(&[("head-cto", "")]);
    let check = check_of(&no_pool, "head-cto");
    assert_eq!(check.issues.len(), 1, "{:?}", check.issues);
    assert_eq!(check.issues[0].severity, Severity::Warning);
    assert!(check.issues[0].message.contains("must name a pool"));

    let ready = config(&[("head-cto", "pool = \"p\"\nmodel = \"P2\"")]);
    assert!(check_of(&ready, "head-cto").issues.is_empty());
    assert!(check_of(&ready, "head-cto model").issues.is_empty());
}
