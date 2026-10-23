use crate::mailspace::MailspaceConfig;
use crate::routing::{Severity, pump_role_checks, validate};

const HARNESS: &str = r#"
[pools.p.harness]
name = "fake"
run = "fake --model {model} @{brief}"
models = ["m/ok"]
"#;

const BANDS: &str = r#"
[pools.p.bands]
P2 = [{ model = "m/ok", reasoning = "high" }]
"#;

/// A pump config whose planner, merge, auditor and gate roles all use pool
/// `p`, with `model` on the roles and `extra` pool tables appended.
fn config(model: &str, extra: &str) -> MailspaceConfig {
    let mut text = String::from("name = \"t\"\n");
    for (name, kind) in [
        ("hand", "hand"),
        ("pump-planner", "planner"),
        ("merge", "merge"),
        ("auditor", "auditor"),
        ("gate", "gate"),
    ] {
        text.push_str(&format!(
            "[[identities]]\nname = \"{name}\"\nkind = \"{kind}\"\nstatus = \"active\"\n\
             pool = \"p\"\n{model}\n"
        ));
    }
    text.push_str(
        "[pools.p]\nprovider = \"local\"\nslots = 2\n[pools.p.local]\nworktree_of = \"/tmp/t\"\n\
         [pump]\nbranch = \"next\"\nto = \"hand\"\n",
    );
    text.push_str(extra);
    toml::from_str(&text).expect("mailspace toml")
}

fn model_failures(config: &MailspaceConfig) -> Vec<(&'static str, String)> {
    pump_role_checks(config)
        .into_iter()
        .filter(|check| check.name.ends_with(" model"))
        .flat_map(|check| {
            check
                .issues
                .into_iter()
                .map(move |issue| (check.name, issue.message))
        })
        .collect()
}

#[test]
fn roles_with_a_band_the_pool_serves_pass() {
    let found = model_failures(&config("model = \"P2-S0\"", &format!("{HARNESS}{BANDS}")));
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_band_with_no_bands_table_names_each_stalled_role_and_the_fix() {
    let found = model_failures(&config("model = \"P2-S0\"", HARNESS));
    let names: Vec<&str> = found.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        [
            "pump-planner model",
            "merge model",
            "auditor model",
            "gate model"
        ]
    );
    let (_, message) = &found[0];
    assert!(
        message.contains("pool p has no model for band P2"),
        "{message}"
    );
    assert!(message.contains("[pools.p.bands]"), "{message}");
}

#[test]
fn a_role_with_no_band_and_no_model_is_reported() {
    let found = model_failures(&config("", &format!("{HARNESS}{BANDS}")));
    assert_eq!(found.len(), 4, "{found:?}");
    assert!(found[0].1.contains("no model"), "{found:?}");
}

#[test]
fn a_pool_with_no_harness_cannot_run_its_roles() {
    let found = model_failures(&config("model = \"P2-S0\"", BANDS));
    assert_eq!(found.len(), 4, "{found:?}");
    assert!(found[1].1.contains("[pools.p.harness]"), "{}", found[1].1);
}

#[test]
fn the_hand_role_needs_no_band() {
    // The pump picks each unit's model from the pool's caps.
    let mut text = config("model = \"P2-S0\"", &format!("{HARNESS}{BANDS}"));
    for role in &mut text.identities {
        if role.name != "hand" {
            continue;
        }
        role.model = None;
    }
    assert!(model_failures(&text).is_empty());
}

#[test]
fn validate_reports_the_findings_as_errors() {
    let issues = validate(&config("model = \"P2-S0\"", HARNESS));
    let found = issues
        .iter()
        .filter(|issue| issue.severity == Severity::Error)
        .filter(|issue| issue.message.contains("pump jobs would stay pending"))
        .count();
    assert_eq!(found, 4, "{issues:?}");
}

#[test]
fn a_role_without_a_gate_is_not_checked_for_one() {
    let mut text = config("model = \"P2-S0\"", &format!("{HARNESS}{BANDS}"));
    text.identities.retain(|role| role.name != "gate");
    assert!(model_failures(&text).is_empty());
}

#[test]
fn roles_with_a_model_slug_in_the_harness_models_pass() {
    let found = model_failures(&config("model = \"m/ok\"", HARNESS));
    assert!(found.is_empty(), "{found:?}");
}
