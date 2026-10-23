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
