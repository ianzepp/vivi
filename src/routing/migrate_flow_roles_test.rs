//! `vivi config migrate` plans the flow roles and their charters.

use toml_edit::DocumentMut;

use crate::mailspace::MailspaceConfig;
use crate::mailspace::shipped::{ADMITTER_CHARTER, RESEARCHER_CHARTER, SLICER_CHARTER};
use crate::routing::{MigrateInputs, MigratePlan, apply_plan_to_document, plan_migrate};

/// A mailspace that already migrated the pump roles but has no flow roles.
const BASE: &str = r#"
name = "t"
id = "11111111-2222-3333-4444-555555555555"

[[identities]]
name = "hand"
kind = "hand"
status = "active"
pool = "glm"

[[identities]]
name = "merge"
kind = "merge"
status = "active"
pool = "glm"

[[identities]]
name = "pump-planner"
kind = "planner"
status = "active"
pool = "glm"

[[identities]]
name = "auditor"
kind = "auditor"
status = "active"
pool = "glm"

[[identities]]
name = "gate"
kind = "gate"
status = "active"
pool = "glm"

[pools.glm]
provider = "local"
slots = 4

[pump]
branch = "next"
to = "hand"
"#;

const FLOW: [&str; 3] = ["researcher", "admitter", "slicer"];

fn plan_of(raw: &str, inputs: &MigrateInputs) -> MigratePlan {
    let config: MailspaceConfig = toml::from_str(raw).expect("mailspace toml");
    plan_migrate(&config, &raw.parse().expect("document"), inputs)
}

fn charters_of(plan: &MigratePlan) -> Vec<(&str, &str)> {
    let mut rows: Vec<_> = plan
        .charters
        .iter()
        .filter(|write| FLOW.contains(&write.role.as_str()))
        .map(|write| (write.role.as_str(), write.text.as_str()))
        .collect();
    rows.sort_unstable();
    rows
}

#[test]
fn a_mailspace_without_flow_roles_plans_three_role_adds_and_three_charters() {
    let plan = plan_of(BASE, &MigrateInputs::default());
    let mut added: Vec<_> = plan
        .new_roles
        .iter()
        .filter(|role| FLOW.contains(&role.name.as_str()))
        .collect();
    added.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(added.len(), 3, "{plan:?}");
    for role in added {
        assert_eq!(role.kind, role.name);
        assert_eq!(role.pool, "glm");
        assert_eq!(role.read_only, Some(true));
    }
    assert_eq!(
        charters_of(&plan),
        vec![
            ("admitter", ADMITTER_CHARTER),
            ("researcher", RESEARCHER_CHARTER),
            ("slicer", SLICER_CHARTER),
        ]
    );
}

#[test]
fn a_second_migrate_of_the_written_result_plans_nothing() {
    let first = plan_of(BASE, &MigrateInputs::default());
    let mut doc: DocumentMut = BASE.parse().expect("document");
    apply_plan_to_document(&mut doc, &first).expect("apply");
    let mut inputs = MigrateInputs::default();
    for write in &first.charters {
        inputs
            .charters
            .insert(write.role.clone(), write.text.clone());
    }
    let after = doc.to_string();
    let again = plan_of(&after, &inputs);
    assert!(
        again.new_roles.is_empty()
            && again.assignments.is_empty()
            && again.charters.is_empty()
            && again.review.is_empty(),
        "{again:?}"
    );
}

#[test]
fn an_existing_flow_role_with_a_pool_and_a_charter_is_left_alone() {
    let raw = format!(
        "{BASE}\n[[identities]]\nname = \"slicer\"\nkind = \"slicer\"\nstatus = \"active\"\npool = \"glm\"\n"
    );
    let mut inputs = MigrateInputs::default();
    inputs
        .charters
        .insert("slicer".into(), "Your own slicer rules.\n".into());
    let plan = plan_of(&raw, &inputs);
    assert!(plan.new_roles.iter().all(|role| role.name != "slicer"));
    assert!(plan.assignments.iter().all(|row| row.role != "slicer"));
    assert!(plan.charters.iter().all(|write| write.role != "slicer"));
    let flow: Vec<_> = charters_of(&plan).iter().map(|row| row.0).collect();
    assert_eq!(flow, vec!["admitter", "researcher"], "{plan:?}");
}

#[test]
fn an_existing_flow_role_without_a_pool_gets_the_hand_pool() {
    let raw = format!(
        "{BASE}\n[[identities]]\nname = \"admitter\"\nkind = \"admitter\"\nstatus = \"active\"\n"
    );
    let plan = plan_of(&raw, &MigrateInputs::default());
    assert!(
        plan.assignments
            .iter()
            .any(|row| row.role == "admitter" && row.pool == "glm"),
        "{plan:?}"
    );
    assert!(plan.new_roles.iter().all(|role| role.name != "admitter"));
}

#[test]
fn new_flow_roles_take_no_hand_band_and_each_asks_for_one_in_review() {
    let raw = BASE.replace(
        "name = \"hand\"\nkind = \"hand\"",
        "name = \"hand\"\nkind = \"hand\"\nmodel = \"P2\"",
    );
    let plan = plan_of(&raw, &MigrateInputs::default());
    for name in FLOW {
        let role = plan
            .new_roles
            .iter()
            .find(|role| role.name == name)
            .expect("flow role added");
        assert_eq!(role.model, None, "{name}");
        let want = format!("set a band for {name}: `vivi role set {name} --model <band>`");
        assert!(
            plan.review.iter().any(|item| *item == want),
            "{name}: {plan:?}"
        );
    }
}
