//! Routing, validate, migrate, and read-only defaults.

use std::collections::BTreeMap;

use crate::mailspace::MailspaceConfig;
use crate::routing::{
    Issue, MigrateInputs, RoleRouting, RoutedPool, RoutedRole, Severity, apply_plan_to_document,
    default_read_only, from_mailspace, merge_lane_count, plan_migrate, require_ok, unified_diff,
    validate,
};

fn cfg(text: &str) -> MailspaceConfig {
    toml::from_str(text).expect("mailspace toml")
}

fn role(name: &str, kind: &str, pool: Option<&str>) -> RoutedRole {
    RoutedRole {
        name: name.into(),
        kind: Some(kind.into()),
        status: "active".into(),
        pool: pool.map(str::to_string),
        read_only: None,
        model: None,
        models: Vec::new(),
    }
}

fn pool(slots: u32) -> RoutedPool {
    RoutedPool {
        slots,
        provider: "local".into(),
        fallback: None,
        caps: BTreeMap::new(),
        models: Vec::new(),
        has_bands: false,
    }
}

#[test]
fn fallback_chain_follows_config_in_order() {
    let config = cfg(r#"
name = "t"
[pools.local]
provider = "local"
slots = 1
fallback = "docker"
[pools.docker]
provider = "docker"
slots = 2
image = "rust:1.97"
fallback = "fly"
[pools.fly]
provider = "fly"
slots = 3
image = "rust:1.97"
[pools.fly.fly]
app = "t-fly"
"#);
    let routing = from_mailspace(&config);
    assert_eq!(routing.pools["local"].fallback.as_deref(), Some("docker"));
    assert_eq!(routing.fallback_chain("local"), ["local", "docker", "fly"]);
    assert!(routing.fallback_chain("missing").is_empty());
}

#[test]
fn fallback_chain_stops_at_a_cycle() {
    let mut a = pool(1);
    a.fallback = Some("b".into());
    let mut b = pool(1);
    b.fallback = Some("a".into());
    let routing = RoleRouting {
        roles: Vec::new(),
        pools: BTreeMap::from([("a".into(), a), ("b".into(), b)]),
    };
    assert_eq!(routing.fallback_chain("a"), ["a", "b"]);
}

#[test]
fn validate_rejects_unknown_fallbacks_and_cycles() {
    let config = cfg(r#"
name = "t"
[pools.a]
provider = "local"
slots = 1
fallback = "missing"
[pools.b]
provider = "local"
slots = 1
fallback = "c"
[pools.c]
provider = "local"
slots = 1
fallback = "b"
"#);
    let issues = validate(&config);
    assert!(
        issues.iter().any(|issue| issue
            .message
            .contains("pool a falls back to unknown pool `missing`")),
        "{issues:?}"
    );
    assert!(
        issues.iter().any(|issue| issue
            .message
            .contains("pool fallback cycle includes [b, c]")),
        "{issues:?}"
    );
}

#[test]
fn read_only_defaults_follow_kind() {
    for kind in [
        "auditor", "triage", "cadence", "steward", "head", "gate", "digest",
    ] {
        assert!(default_read_only(Some(kind)), "{kind}");
    }
    for kind in ["hand", "merge", "lint"] {
        assert!(!default_read_only(Some(kind)), "{kind}");
    }
    assert!(!default_read_only(None));
}

#[test]
fn merge_lanes_follow_the_merge_role_pool_slots() {
    let routing = RoleRouting {
        roles: vec![role("merge", "merge", Some("host"))],
        pools: BTreeMap::from([("host".into(), pool(4))]),
    };
    assert_eq!(merge_lane_count(&routing), 4);
}

#[test]
fn merge_lanes_fall_back_to_one_slot_when_unassigned() {
    let routing = RoleRouting {
        roles: vec![role("merge", "merge", None)],
        pools: BTreeMap::new(),
    };
    assert_eq!(merge_lane_count(&routing), 1);
}

#[test]
fn auditor_lookup_finds_named_role() {
    let routing = RoleRouting {
        roles: vec![role("auditor", "auditor", Some("glm"))],
        pools: BTreeMap::new(),
    };
    let found = routing.auditor().expect("auditor role");
    assert_eq!(found.name, "auditor");
}

#[test]
fn auditor_lookup_finds_kind_role() {
    let routing = RoleRouting {
        roles: vec![
            role("auditor-1", "hand", None),
            role("auditor-2", "auditor", None),
        ],
        pools: BTreeMap::new(),
    };
    let found = routing.auditor().expect("kind auditor role");
    assert_eq!(found.name, "auditor-2");
}

#[test]
fn auditor_lookup_missing_returns_none() {
    let routing = RoleRouting {
        roles: vec![role("hand", "hand", None), role("merge", "merge", None)],
        pools: BTreeMap::new(),
    };
    assert!(routing.auditor().is_none());
}

#[test]
fn validate_errors_on_unknown_pool_and_missing_hand_pool() {
    let config = cfg(r#"
name = "t"
[[identities]]
name = "hand"
kind = "hand"
pool = "missing"
[pump]
branch = "next"
to = "hand"
"#);
    let issues = validate(&config);
    assert!(
        issues.iter().any(|issue| {
            issue.severity == Severity::Error && issue.message.contains("unknown pool")
        }),
        "{issues:?}"
    );
    assert!(require_ok(&issues).is_err());
}

#[test]
fn validate_requires_an_active_pool_planner_role_for_pumps() {
    let config = cfg(r#"
name = "t"
[[identities]]
name = "hand"
kind = "hand"
pool = "here"
[pools.here]
provider = "local"
slots = 1
[pump]
branch = "next"
to = "hand"
"#);
    let issues = validate(&config);
    assert!(
        issues.iter().any(|issue| {
            issue.severity == Severity::NotReady
                && issue
                    .message
                    .contains("active pump-planner role assigned to a pool")
        }),
        "{issues:?}"
    );
}

#[test]
fn validate_rejects_missing_merge_role_even_with_legacy_merge_settings() {
    let config = cfg(r#"
name = "t"
[[identities]]
name = "hand"
kind = "hand"
pool = "here"
[[identities]]
name = "pump-planner"
kind = "planner"
pool = "here"
[pools.here]
provider = "local"
slots = 1
[pump]
branch = "next"
to = "hand"
[pump.merge]
lanes = 2
"#);
    let issues = validate(&config);
    assert!(
        issues.iter().any(|issue| {
            issue.severity == Severity::NotReady
                && issue
                    .message
                    .contains("active merge role assigned to a pool")
        }),
        "{issues:?}"
    );
}

#[test]
fn validate_rejects_disabled_and_unassigned_merge_roles() {
    // A paused merge role is wrong; one with no pool is only missing.
    for (merge, expected, severity) in [
        (
            "[[identities]]\nname = \"merge\"\nkind = \"merge\"\nstatus = \"paused\"\npool = \"here\"",
            "must be active",
            Severity::Error,
        ),
        (
            "[[identities]]\nname = \"merge\"\nkind = \"merge\"\nstatus = \"active\"",
            "must name a pool",
            Severity::NotReady,
        ),
    ] {
        let config = cfg(&format!(
            "name = \"t\"\n[[identities]]\nname = \"hand\"\nkind = \"hand\"\npool = \"here\"\n[[identities]]\nname = \"pump-planner\"\nkind = \"planner\"\npool = \"here\"\n[[identities]]\nname = \"auditor\"\nkind = \"auditor\"\npool = \"here\"\n{merge}\n[pools.here]\nprovider = \"local\"\nslots = 1\n[pump]\nbranch = \"next\"\nto = \"hand\"\n"
        ));
        let issues = validate(&config);
        assert!(
            issues
                .iter()
                .any(|issue| { issue.severity == severity && issue.message.contains(expected) }),
            "expected {expected:?} in {issues:?}"
        );
    }
}

#[test]
fn validate_reports_a_hand_without_a_pool_not_ready_and_bands_without_a_table() {
    let config = cfg(r#"
name = "t"
[[identities]]
name = "hand"
kind = "hand"
model = "P2-S0"
[[identities]]
name = "canary"
kind = "hand"
pool = "glm"
model = "P2-S0"
[[identities]]
name = "merge"
kind = "merge"
pool = "pumps"
model = "P2-S0"
[[identities]]
name = "pump-planner"
kind = "planner"
pool = "pumps"
model = "P2-S0"
[[identities]]
name = "auditor"
kind = "auditor"
pool = "pumps"
model = "P2-S0"
[pools.glm]
provider = "local"
slots = 2
[pools.pumps]
provider = "local"
slots = 2
[pools.pumps.harness]
name = "fake"
run = "fake --model {model} @{brief}"
models = ["m/ok"]
[pools.pumps.bands]
P2 = [{ model = "m/ok", reasoning = "high" }]
[pump]
branch = "next"
to = "hand"
[pump.merge]
lanes = 2
"#);
    let issues = validate(&config);
    assert!(
        issues
            .iter()
            .any(|issue| issue.severity == Severity::NotReady
                && issue.message.contains("role hand has no pool")),
        "{issues:?}"
    );
    assert!(
        !issues.iter().any(|issue| issue.message.contains("legacy")),
        "{issues:?}"
    );
    assert!(
        issues
            .iter()
            .any(|issue| issue.message.contains("[pools.glm] has no bands table")),
        "{issues:?}"
    );
    assert!(require_ok(&issues).is_ok());
}

#[test]
fn validate_errors_when_a_role_pool_has_a_band_and_no_fallback() {
    let config = cfg(r#"
name = "t"
[[identities]]
name = "hand"
kind = "hand"
pool = "glm"
model = "P2-S0"
[pools.glm]
provider = "local"
slots = 2
[pump]
branch = "next"
to = "hand"
"#);
    let issues = validate(&config);
    assert!(
        issues.iter().any(|issue| {
            issue.severity == Severity::NotReady && issue.message.contains("has no bands table")
        }),
        "{issues:?}"
    );
}

#[test]
fn from_mailspace_reads_role_pool_and_models() {
    let config = cfg(r#"
name = "t"
[[identities]]
name = "hand"
kind = "hand"
pool = "glm"
models = ["m/a", "P2-S0"]
[pools.glm]
provider = "local"
slots = 10
[pools.glm.caps]
"m/a" = 2
"#);
    let routing = from_mailspace(&config);
    let hand = routing.hand("hand").unwrap();
    assert_eq!(hand.pool.as_deref(), Some("glm"));
    assert_eq!(hand.models, ["m/a", "P2-S0"]);
    assert_eq!(routing.pools["glm"].slots, 10);
    assert_eq!(routing.pools["glm"].caps["m/a"], 2);
}

#[test]
fn migrate_assigns_hand_and_merge_and_folds_caps() {
    let raw = r#"
name = "t"
[[identities]]
name = "hand"
kind = "hand"
model = "P2-S0"
[[identities]]
name = "merge"
kind = "hand"
[pools.glm]
provider = "local"
slots = 10
[pump]
branch = "next"
to = "hand"
[pump.dispatch]
default_kind = "plain"
[[pump.dispatch.lanes]]
model = "m/a"
cap = 2
[pump.dispatch.kinds.plain]
pools = ["glm"]
[pump.merge]
lanes = 4
"#;
    let config = cfg(raw);
    let plan = plan_migrate(&config, &raw.parse().unwrap(), &MigrateInputs::default());
    assert!(
        plan.assignments
            .iter()
            .any(|row| row.role == "hand" && row.pool == "glm"),
        "{plan:?}"
    );
    assert!(
        plan.assignments
            .iter()
            .any(|row| row.role == "merge" && row.pool == "host"),
        "{plan:?}"
    );
    assert!(!plan.new_roles.iter().any(|role| role.name == "gate"));
    assert!(
        plan.new_pools
            .iter()
            .any(|pool| pool.name == "host" && pool.slots == 4 && pool.provider == "local")
    );
    assert!(
        plan.caps
            .iter()
            .any(|cap| cap.pool == "glm" && cap.model == "m/a" && cap.cap == 2)
    );
}

#[test]
fn migrate_apply_writes_role_pool_without_dropping_comments() {
    let raw = r#"
# keep-me
name = "t"

[[identities]]
name = "hand"
kind = "hand"

[[identities]]
name = "merge"
kind = "merge"

[pools.glm]
provider = "local"
slots = 2

[pump]
branch = "next"
to = "hand"

[pump.dispatch]
default_kind = "plain"

[[pump.dispatch.lanes]]
model = "m/a"
cap = 1

[pump.dispatch.kinds.plain]
pools = ["glm"]

[pump.merge]
lanes = 2
"#;
    let config: MailspaceConfig = toml::from_str(raw).unwrap();
    let plan = plan_migrate(&config, &raw.parse().unwrap(), &MigrateInputs::default());
    let mut doc = raw.parse().unwrap();
    apply_plan_to_document(&mut doc, &plan).unwrap();
    let after = doc.to_string();
    assert!(after.contains("# keep-me"), "{after}");
    assert!(after.contains("pool = \"glm\""), "{after}");
    assert!(after.contains("[pools.host]"), "{after}");
}

#[test]
fn migrate_rewrites_the_legacy_planner_split_timeout_key() {
    let raw = r#"
# keep-me
name = "t"

[pump]
branch = "next"

[pump.planner]
run = "planner-run"
split_timeout = 1800
"#;
    let config: MailspaceConfig = toml::from_str(raw).expect("mailspace toml");
    let plan = plan_migrate(&config, &raw.parse().unwrap(), &MigrateInputs::default());
    assert_eq!(plan.renames.len(), 1, "{:?}", plan.renames);
    assert_eq!(plan.renames[0].from, "split_timeout");
    assert_eq!(plan.renames[0].to, "split_hard_cap");
    let mut doc = raw.parse::<toml_edit::DocumentMut>().unwrap();
    apply_plan_to_document(&mut doc, &plan).unwrap();
    let after = doc.to_string();
    assert!(after.contains("split_hard_cap = 1800"), "{after}");
    assert!(!after.contains("split_timeout"), "{after}");
    assert!(after.contains("# keep-me"), "{after}");

    let both = r#"
name = "t"

[pump]
branch = "next"

[pump.planner]
run = "planner-run"
split_hard_cap = 2700
split_timeout = 1800
"#;
    let config: MailspaceConfig = toml::from_str(both).expect("mailspace toml");
    let plan = plan_migrate(&config, &both.parse().unwrap(), &MigrateInputs::default());
    assert_eq!(plan.renames.len(), 1, "{:?}", plan.renames);
    let mut doc = both.parse::<toml_edit::DocumentMut>().unwrap();
    apply_plan_to_document(&mut doc, &plan).unwrap();
    let after = doc.to_string();
    assert!(after.contains("split_hard_cap = 2700"), "{after}");
    assert!(!after.contains("split_timeout"), "{after}");
}

#[test]
fn unified_diff_names_the_file() {
    let diff = unified_diff("a\n", "b\n", "mailspace.toml");
    assert!(diff.contains("--- mailspace.toml"));
    assert!(diff.contains("+b"));
}

#[test]
fn issues_distinguish_error_warning_and_not_ready() {
    let error = Issue {
        severity: Severity::Error,
        message: "bad".into(),
        item: None,
    };
    let warning = Issue {
        severity: Severity::Warning,
        message: "old".into(),
        item: None,
    };
    let missing = Issue::not_ready("gate pool", "no gate pool");
    assert_eq!(error.severity, Severity::Error);
    assert_eq!(warning.severity, Severity::Warning);
    assert_eq!(missing.severity, Severity::NotReady);
    assert_eq!(missing.item.as_deref(), Some("gate pool"));
    assert!(require_ok(&[missing.clone(), warning]).is_ok());
    assert_eq!(
        crate::routing::not_ready_summary(&[missing]),
        Some(String::from("not ready: gate pool"))
    );
}

#[test]
fn faber_fixture_migrates_hand_to_glm_and_merge_to_host() {
    let raw = include_str!("../../tests/fixtures/faber-mailspace.toml");
    let config: MailspaceConfig = toml::from_str(raw).expect("faber fixture parses");
    let plan = plan_migrate(&config, &raw.parse().unwrap(), &MigrateInputs::default());
    assert!(
        plan.assignments
            .iter()
            .any(|row| row.role == "hand" && row.pool == "glm"),
        "{}",
        crate::routing::render_plan(&plan)
    );
    assert!(
        plan.assignments
            .iter()
            .any(|row| row.role == "merge" && row.pool == "host")
    );
    assert!(!plan.new_roles.iter().any(|role| role.name == "gate"));
    assert!(
        plan.new_pools
            .iter()
            .any(|pool| pool.name == "host" && pool.slots == 1)
    );
    assert!(
        plan.caps
            .iter()
            .any(|cap| cap.pool == "glm" && cap.model.contains("deepseek") && cap.cap == 10)
    );
    let mut doc = raw.parse().unwrap();
    apply_plan_to_document(&mut doc, &plan).unwrap();
    let migrated: MailspaceConfig = toml::from_str(&doc.to_string()).unwrap();
    let issues = validate(&migrated);
    assert!(require_ok(&issues).is_ok(), "{issues:?}");
}
