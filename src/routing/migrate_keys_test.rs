//! Retired merge keys under `[pump.targets.<id>.merge]` are planned for removal.

use crate::mailspace::MailspaceConfig;
use crate::routing::{MigrateInputs, MigratePlan, Severity, plan_migrate};

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
[pump.workspace]
lane_gib = 8
margin_gib = 4
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
        "pump.workspace",
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

/// The five pump keys scheduled for removal while the pump still reads them.
const PLANNED: [&str; 5] = [
    "pump.from",
    "pump.reply_from",
    "pump.board.from",
    "pump.board.reply_to",
    "pump.dispatch.from",
];

#[test]
fn pump_keys_planned_for_removal_warn_under_their_dotted_names() {
    let plan = plan_of(
        r#"
from = "operator"
reply_from = "mind"
[pump.board]
from = ["operator@x"]
reply_to = "operator@x"
[pump.dispatch]
from = "origin/main"
"#,
    );
    for dotted in PLANNED {
        let (table, key) = dotted.rsplit_once('.').expect("dotted key");
        let row = plan
            .removals
            .iter()
            .find(|row| row.table == table && row.key == key)
            .unwrap_or_else(|| panic!("{dotted} is not planned for removal: {plan:?}"));
        assert_eq!(row.severity, Severity::Warning, "{row:?}");
        assert_eq!(row.issue.as_deref(), Some(dotted), "{row:?}");
        assert!(!row.behaviour_change, "{row:?}");
        assert!(row.reason.starts_with("planned for removal:"), "{row:?}");
    }
    // Nothing else is flagged: the pump keys vivi keeps reading (every,
    // pull, push_since, budget, parallel, format, remote_limit, hand_rules)
    // and the unrelated retired settings stay out of the warning set.
    let mut planned: Vec<&str> = plan
        .removals
        .iter()
        .map(|row| row.issue.as_deref().expect("issue name"))
        .collect();
    planned.sort_unstable();
    let mut expected = PLANNED.to_vec();
    expected.sort_unstable();
    assert_eq!(planned, expected, "{plan:?}");
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
        "[pump.workspace]",
        "lane_gib",
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
