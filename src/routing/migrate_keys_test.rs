//! Retired merge keys under `[pump.targets.<id>.merge]` are planned for removal.

use crate::mailspace::MailspaceConfig;
use crate::routing::{MigrateInputs, MigratePlan, plan_migrate};

const BASE: &str = r#"
name = "t"

[[identities]]
name = "hand"
kind = "hand"
status = "active"
pool = "glm"

[pools.glm]
provider = "local"
slots = 2

[pump]
to = "hand"
"#;

fn plan_of(extra: &str) -> MigratePlan {
    let raw = format!("{BASE}{extra}");
    let config: MailspaceConfig = toml::from_str(&raw).expect("mailspace toml");
    plan_migrate(
        &config,
        &raw.parse().expect("document"),
        &MigrateInputs::default(),
    )
}

#[test]
fn a_target_merge_table_with_all_seven_keys_plans_seven_removals() {
    let plan = plan_of(
        r#"
[pump.targets.other]
repo = "/tmp/other"
[pump.targets.other.merge]
batch = 2
need_parallel = 2
shared_checks = ["true"]
checks_for = { a = ["true"] }
proof_timeout = 60
remote_wait = 60
lanes = 2
"#,
    );
    let mut keys: Vec<&str> = plan
        .removals
        .iter()
        .filter(|removal| removal.table == "pump.targets.other.merge")
        .map(|removal| removal.key.as_str())
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "batch",
            "checks_for",
            "lanes",
            "need_parallel",
            "proof_timeout",
            "remote_wait",
            "shared_checks"
        ],
        "{plan:?}"
    );
}

#[test]
fn a_target_without_a_merge_table_plans_no_merge_removals() {
    let plan = plan_of("\n[pump.targets.other]\nrepo = \"/tmp/other\"\n");
    assert!(
        plan.removals
            .iter()
            .all(|removal| !removal.table.ends_with(".merge")),
        "{plan:?}"
    );
}

/// Every key vivi 21 does not read, written in the table that holds it.
const REFUSED: &str = r#"
reserved_file = "reserved.txt"
fix_prefix = "fx"
[pump.board]
planners_max = 2
plan_timeout = 60
[pump.planner]
run = "x"
charter = "c.md"
[pump.proof]
allow = ["cargo"]
[[pump.plan.proof_rules]]
scope = "src"
proof = "cargo test"
[pump.alarm]
merge_idle_minutes = 5
[pump.dispatch]
default_kind = "plain"
max_attempts = 2
per_tick = 3
transient_max = 1
transient_backoff = 10
overdue_grace = 30
last_resort = "m/ok"
[[pump.dispatch.lanes]]
model = "m/ok"
cap = 1
[pump.dispatch.kinds.plain]
pools = ["glm"]
[pump.merge]
repair_budget = 5
[pump.targets.other]
repo = "/tmp/other"
[pump.targets.other.merge]
repair_budget = 4
"#;

#[test]
fn every_key_vivi_21_does_not_read_is_planned_for_removal_with_a_reason() {
    let plan = plan_of(REFUSED);
    let mut found: Vec<String> = plan
        .removals
        .iter()
        .map(|row| format!("{}.{}", row.table, row.key))
        .collect();
    found.sort_unstable();
    let mut expected = vec![
        "pump.reserved_file",
        "pump.fix_prefix",
        "pump.board.planners_max",
        "pump.board.plan_timeout",
        "pump.planner",
        "pump.proof",
        "pump.plan",
        "pump.alarm",
        "pump.dispatch.default_kind",
        "pump.dispatch.max_attempts",
        "pump.dispatch.per_tick",
        "pump.dispatch.transient_max",
        "pump.dispatch.transient_backoff",
        "pump.dispatch.overdue_grace",
        "pump.dispatch.last_resort",
        "pump.dispatch.lanes",
        "pump.dispatch.kinds",
        "pump.merge.repair_budget",
        "pump.targets.other.merge.repair_budget",
    ];
    expected.sort_unstable();
    assert_eq!(found, expected, "{plan:?}");
    assert!(
        plan.removals
            .iter()
            .all(|row| row.reason.starts_with("not read by vivi 21: ")),
        "{plan:?}"
    );
}

#[test]
fn the_read_pump_keys_are_not_planned_for_removal() {
    let plan = plan_of(
        r#"
hand_rules = "rules.md"
[pump.board]
from = ["operator@x"]
every = 120
pull = false
reply_to = "operator@x"
push_since = "30m"
[pump.dispatch]
budget = 600
parallel = 2
from = "origin/main"
[pump.merge]
format = "cargo fmt"
remote_limit = 1800
"#,
    );
    assert!(plan.removals.is_empty(), "{plan:?}");
}

#[test]
fn applying_the_plan_leaves_none_of_the_refused_keys() {
    let raw = format!("{BASE}{REFUSED}");
    let config: MailspaceConfig = toml::from_str(&raw).expect("mailspace toml");
    let mut doc: toml_edit::DocumentMut = raw.parse().expect("document");
    let plan = plan_migrate(&config, &doc, &MigrateInputs::default());
    crate::routing::apply_plan_to_document(&mut doc, &plan).expect("apply");
    let after = doc.to_string();
    for gone in [
        "reserved_file",
        "fix_prefix",
        "planners_max",
        "plan_timeout",
        "[pump.planner]",
        "[pump.proof]",
        "proof_rules",
        "[pump.alarm]",
        "default_kind",
        "max_attempts",
        "per_tick",
        "transient_",
        "overdue_grace",
        "last_resort",
        "lanes",
        "kinds",
        "repair_budget",
    ] {
        assert!(!after.contains(gone), "{gone} still in:\n{after}");
    }
    assert!(after.contains("repo = \"/tmp/other\""), "{after}");
}
