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
