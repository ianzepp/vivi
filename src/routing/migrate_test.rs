//! `vivi config migrate`: planner and merge roles, retired keys, local pool
//! roots, and the merge-kind validate error.

use std::path::Path;

use toml_edit::DocumentMut;

use crate::mailspace::MailspaceConfig;
use crate::mailspace::shipped::{AUDITOR_CHARTER, MERGE_CHARTER, PUMP_PLANNER_CHARTER};
use crate::routing::{
    LegacyCharter, MigrateInputs, MigratePlan, Severity, apply_plan_to_document, plan_migrate,
    render_plan, validate,
};

/// A 15.0.0-shaped config: roles without pools, legacy `[pump]` routing,
/// the retired keys, and a `hand` / `merge` pair the old pump used.
const V15: &str = r#"
# keep-me
name = "t"
id = "11111111-2222-3333-4444-555555555555"

[[identities]]
name = "hand"
kind = "hand"
status = "active"
model = "P2-S0"

[[identities]]
name = "merge"
kind = "hand"
labels = ["merge"]
status = "active"
model = "P2-S0"

[[identities]]
name = "planner"
kind = "planner"
status = "active"

[[identities]]
name = "auditor"
kind = "auditor"
status = "active"
model = "P2-S0"

[pools.glm]
provider = "local"
slots = 10
build_dir = "target"

[pools.glm.local]
worktree_of = "~/work/t"

[pools.glm.harness]
name = "fake"
run = "fake --model {model} @{brief}"
models = ["m/ok"]
env = ["SOME_KEY"]

[pools.glm.committer]
name = "bot"
email = "bot@example.com"

[pools.glm.bands]
P2 = [{ model = "m/ok", reasoning = "high" }]

[pump]
branch = "next"
to = "hand"
priority_file = "priority.txt"

[pump.planner]
run = "pi -p"
charter = "/tmp/planner-charter.md"
split_hard_cap = 1500  # keep-inline

[pump.dispatch]
default_kind = "plain"

[pump.dispatch.kinds.plain]
pools = ["glm"]

[pump.merge]
batch = 4
lanes = 3

[pump.gate]
command = "cargo test"
every_merges = 8
every_seconds = 3600
pools = ["glm"]
"#;

/// A mini config's pool `here` made runnable: a harness and a
/// P2 band, with every role pointed at it given that band.
fn runnable(text: &str) -> String {
    text.replace("pool = \"here\"\n", "pool = \"here\"\nmodel = \"P2\"\n")
        + "[pools.here.harness]\nname = \"fake\"\nrun = \"fake --model {model} @{brief}\"\n\
           models = [\"m/ok\"]\n\
           [pools.here.bands]\nP2 = [{ model = \"m/ok\", reasoning = \"high\" }]\n"
}

fn plan_with(raw: &str, inputs: &MigrateInputs) -> MigratePlan {
    let config: MailspaceConfig = toml::from_str(raw).expect("mailspace toml");
    plan_migrate(&config, &raw.parse().expect("document"), inputs)
}

fn plan_of(raw: &str) -> MigratePlan {
    plan_with(raw, &MigrateInputs::default())
}

/// The plan and the document text after applying it.
fn migrate(raw: &str) -> (MigratePlan, String) {
    migrate_with(raw, &MigrateInputs::default())
}

fn migrate_with(raw: &str, inputs: &MigrateInputs) -> (MigratePlan, String) {
    let plan = plan_with(raw, inputs);
    let mut doc: DocumentMut = raw.parse().expect("document");
    apply_plan_to_document(&mut doc, &plan).expect("apply");
    (plan, doc.to_string())
}

/// The charters on disk after the plan's charter writes.
fn written(inputs: &MigrateInputs, plan: &MigratePlan) -> MigrateInputs {
    let mut next = inputs.clone();
    for write in &plan.charters {
        next.charters.insert(write.role.clone(), write.text.clone());
    }
    next
}

fn legacy(text: &str) -> MigrateInputs {
    MigrateInputs {
        legacy_planner_charter: Some(LegacyCharter {
            path: "/tmp/planner-charter.md".into(),
            text: text.into(),
        }),
        ..MigrateInputs::default()
    }
}

/// The charter writes for `role`.
fn writes_for<'a>(plan: &'a MigratePlan, role: &str) -> Vec<&'a crate::routing::CharterWrite> {
    plan.charters
        .iter()
        .filter(|write| write.role == role)
        .collect()
}

fn without_id(raw: &str) -> String {
    raw.replace("id = \"11111111-2222-3333-4444-555555555555\"\n", "")
}

fn role_of<'a>(config: &'a MailspaceConfig, name: &str) -> &'a crate::mailspace::LocalIdentity {
    config
        .identities
        .iter()
        .find(|role| role.name == name)
        .unwrap_or_else(|| panic!("no role {name}"))
}

#[test]
fn pump_planner_is_created_on_the_hand_pool_and_the_planner_seat_is_untouched() {
    let (plan, after) = migrate(V15);
    let added = plan
        .new_roles
        .iter()
        .find(|role| role.name == "pump-planner");
    let added = added.expect("pump-planner added");
    assert_eq!(
        (added.kind.as_str(), added.pool.as_str()),
        ("planner", "glm")
    );
    assert_eq!(added.read_only, None);
    assert!(plan.assignments.iter().all(|row| row.role != "planner"));
    assert!(plan.new_roles.iter().all(|role| role.name != "planner"));
    let migrated: MailspaceConfig = toml::from_str(&after).unwrap();
    let pump_planner = role_of(&migrated, "pump-planner");
    assert_eq!(pump_planner.kind.as_deref(), Some("planner"));
    assert_eq!(pump_planner.status, "active");
    assert_eq!(pump_planner.pool.as_deref(), Some("glm"));
    let seat = role_of(&migrated, "planner");
    assert_eq!(seat.pool, None);
    assert_eq!(seat.kind.as_deref(), Some("planner"));
}

#[test]
fn pump_planner_follows_a_hand_that_already_has_a_pool() {
    let raw = V15
        .replace(
            "name = \"hand\"\nkind = \"hand\"",
            "name = \"hand\"\nkind = \"hand\"\npool = \"other\"",
        )
        .replace(
            "[pools.glm]",
            "[pools.other]\nprovider = \"local\"\nslots = 1\n\n[pools.glm]",
        );
    let plan = plan_of(&raw);
    let added = plan
        .new_roles
        .iter()
        .find(|role| role.name == "pump-planner");
    assert_eq!(
        added.map(|role| role.pool.as_str()),
        Some("other"),
        "{plan:?}"
    );
}

#[test]
fn an_existing_pump_planner_keeps_its_pool_or_gets_the_hand_pool() {
    let with_role = |extra: &str| {
        V15.replace(
            "[pools.glm]",
            &format!("[[identities]]\nname = \"pump-planner\"\nkind = \"planner\"\nstatus = \"active\"\n{extra}\n[pools.glm]"),
        )
    };
    let plan = plan_of(&with_role(""));
    assert!(
        plan.new_roles
            .iter()
            .all(|role| role.name != "pump-planner")
    );
    assert!(
        plan.assignments
            .iter()
            .any(|row| row.role == "pump-planner" && row.pool == "glm"),
        "{plan:?}"
    );
    let plan = plan_of(&with_role("pool = \"mine\"\n"));
    assert!(
        plan.assignments
            .iter()
            .all(|row| row.role != "pump-planner")
    );
}

#[test]
fn pump_planner_without_a_hand_pool_to_copy_is_reported_not_invented() {
    let raw = "name = \"t\"\n[pump]\nbranch = \"next\"\nto = \"hand\"\n";
    let (plan, after) = migrate(raw);
    assert!(
        plan.new_roles
            .iter()
            .all(|role| role.name != "pump-planner")
    );
    assert!(plan.charters.is_empty());
    assert!(
        plan.review.iter().any(|item| item
            .starts_with("pump-planner needs a pool: run vivi role pool pump-planner <pool>")),
        "{plan:?}"
    );
    assert!(!after.contains("pump-planner"), "{after}");
}

#[test]
fn pump_planner_gets_the_text_of_the_old_planner_charter_file() {
    let inputs = legacy("You are the project planner.\nPlan well.\n");
    let (plan, _) = migrate_with(V15, &inputs);
    assert_eq!(writes_for(&plan, "pump-planner").len(), 1, "{plan:?}");
    let write = writes_for(&plan, "pump-planner")[0];
    assert_eq!(write.role, "pump-planner");
    assert_eq!(write.origin, "/tmp/planner-charter.md");
    assert_eq!(write.text, "You are the project planner.\nPlan well.\n");
    let text = render_plan(&plan);
    assert!(
        text.contains("set role pump-planner charter from /tmp/planner-charter.md (2 lines)"),
        "{text}"
    );
    assert!(
        plan.review
            .iter()
            .all(|item| !item.contains("is not a readable"))
    );
    let copied = plan
        .review
        .iter()
        .find(|item| item.contains("was copied from /tmp/planner-charter.md"));
    assert!(
        copied.is_some_and(|item| item.contains("PUMP-PLAN-BEGIN")),
        "{plan:?}"
    );
}

#[test]
fn pump_planner_falls_back_to_the_shipped_charter() {
    let (plan, _) = migrate(V15);
    let writes = writes_for(&plan, "pump-planner");
    assert_eq!(writes.len(), 1, "{plan:?}");
    assert_eq!(writes[0].text, PUMP_PLANNER_CHARTER);
    assert_eq!(writes[0].origin, "the shipped default");
    assert!(
        plan.review.iter().any(|item| item
            .contains("[pump.planner] charter /tmp/planner-charter.md is not a readable")),
        "{plan:?}"
    );
    let text = render_plan(&plan);
    assert!(
        text.contains("set role pump-planner charter from the shipped default ("),
        "{text}"
    );
}

#[test]
fn a_pump_planner_charter_that_has_text_is_never_replaced() {
    let raw = V15.replace(
        "[pools.glm]",
        "[[identities]]\nname = \"pump-planner\"\nkind = \"planner\"\nstatus = \"active\"\npool = \"glm\"\nmodel = \"P2-S0\"\n\n[pools.glm]",
    );
    let mut inputs = legacy("legacy text\n");
    inputs
        .charters
        .insert("pump-planner".into(), "my own charter\n".into());
    assert!(writes_for(&plan_with(&raw, &inputs), "pump-planner").is_empty());
    inputs.charters.insert("pump-planner".into(), "  \n".into());
    assert_eq!(
        writes_for(&plan_with(&raw, &inputs), "pump-planner").len(),
        1
    );
}

#[test]
fn the_planner_seat_charter_is_not_read_for_pump_planner() {
    let mut inputs = MigrateInputs::default();
    inputs
        .charters
        .insert("planner".into(), "Report to the Mind.\n".into());
    let plan = plan_with(V15, &inputs);
    let writes = writes_for(&plan, "pump-planner");
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].text, PUMP_PLANNER_CHARTER);
}

#[test]
fn a_merge_charter_that_is_not_the_shipped_one_gets_a_review_line() {
    let mut inputs = MigrateInputs::default();
    inputs.charters.insert(
        "merge".into(),
        "Your lane (worktrees/merge/) is the merge lane.\nYou are the only writer to main.\n"
            .into(),
    );
    let plan = plan_with(V15, &inputs);
    let item = plan
        .review
        .iter()
        .find(|item| item.starts_with("merge role merge has its own charter"))
        .unwrap_or_else(|| panic!("{plan:?}"));
    assert!(
        item.contains("Your lane (worktrees/merge/) is the merge lane."),
        "{item}"
    );
    assert!(item.contains("system prompt"), "{item}");
    assert!(item.contains("references/charters/merge.md"), "{item}");
    assert!(plan.charters.iter().all(|write| write.role != "merge"));

    inputs.charters.insert("merge".into(), MERGE_CHARTER.into());
    let plan = plan_with(V15, &inputs);
    assert!(
        plan.review
            .iter()
            .all(|item| !item.contains("has its own charter"))
    );
    inputs.charters.insert("merge".into(), "  \n".into());
    let plan = plan_with(V15, &inputs);
    assert!(
        plan.review
            .iter()
            .all(|item| !item.contains("has its own charter"))
    );
}

#[test]
fn a_missing_merge_role_is_added_on_the_host_pool() {
    let raw = V15.replace(
        "[[identities]]\nname = \"merge\"\nkind = \"hand\"\nlabels = [\"merge\"]\nstatus = \"active\"\nmodel = \"P2-S0\"\n\n",
        "",
    );
    let (plan, after) = migrate(&raw);
    let added = plan.new_roles.iter().find(|role| role.name == "merge");
    let added = added.expect("merge role added");
    assert_eq!(
        (added.kind.as_str(), added.pool.as_str()),
        ("merge", "host")
    );
    assert!(
        plan.new_pools
            .iter()
            .any(|pool| pool.name == "host" && pool.slots == 3),
        "{plan:?}"
    );
    let migrated: MailspaceConfig = toml::from_str(&after).unwrap();
    let merge = migrated.identities.iter().find(|r| r.name == "merge");
    let merge = merge.expect("merge in the document");
    assert_eq!(merge.kind.as_deref(), Some("merge"));
    assert_eq!(merge.status, "active");
    assert_eq!(merge.pool.as_deref(), Some("host"));
    assert!(
        plan.review
            .iter()
            .any(|item| item.contains("pool host: merge lanes want persistent warm slots")),
        "{plan:?}"
    );
}

#[test]
fn a_merge_role_of_the_wrong_kind_is_changed_and_keeps_its_other_fields() {
    let (plan, after) = migrate(V15);
    assert_eq!(plan.kind_changes.len(), 1, "{plan:?}");
    let change = &plan.kind_changes[0];
    assert_eq!(change.role, "merge");
    assert_eq!(change.from.as_deref(), Some("hand"));
    assert_eq!(change.to, "merge");
    let migrated: MailspaceConfig = toml::from_str(&after).unwrap();
    let merge = migrated.identities.iter().find(|r| r.name == "merge");
    let merge = merge.expect("merge role kept");
    assert_eq!(merge.kind.as_deref(), Some("merge"));
    assert_eq!(merge.labels, ["merge"]);
    assert_eq!(merge.pool.as_deref(), Some("host"));
    let text = render_plan(&plan);
    assert!(
        text.contains("set role merge kind=merge (was hand)"),
        "{text}"
    );
}

#[test]
fn validate_requires_pump_planner_and_ignores_a_role_named_planner() {
    let base = "name = \"t\"\n[[identities]]\nname = \"hand\"\nkind = \"hand\"\npool = \"here\"\n[[identities]]\nname = \"merge\"\nkind = \"merge\"\npool = \"here\"\n[[identities]]\nname = \"auditor\"\nkind = \"auditor\"\npool = \"here\"\n[[identities]]\nname = \"researcher\"\nkind = \"researcher\"\npool = \"here\"\n[[identities]]\nname = \"admitter\"\nkind = \"admitter\"\npool = \"here\"\n[[identities]]\nname = \"slicer\"\nkind = \"slicer\"\npool = \"here\"\n[pools.here]\nprovider = \"local\"\nslots = 1\n[pump]\nbranch = \"next\"\nto = \"hand\"\n";
    // Everything that stops the pump: wrong values and missing items.
    let errors = |text: &str| -> Vec<String> {
        let config: MailspaceConfig = toml::from_str(&runnable(text)).unwrap();
        validate(&config)
            .into_iter()
            .filter(|issue| issue.severity != Severity::Warning)
            .map(|issue| issue.message)
            .collect()
    };
    let seat =
        format!("{base}[[identities]]\nname = \"planner\"\nkind = \"planner\"\npool = \"here\"\n");
    let found = errors(&seat);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("pump-planner"), "{found:?}");
    assert!(found[0].contains("vivi config migrate"), "{found:?}");
    let kind_only =
        format!("{base}[[identities]]\nname = \"p2\"\nkind = \"planner\"\npool = \"here\"\n");
    assert_eq!(errors(&kind_only).len(), 1);
    let ok = format!(
        "{base}[[identities]]\nname = \"pump-planner\"\nkind = \"planner\"\npool = \"here\"\n"
    );
    assert!(errors(&ok).is_empty(), "{:?}", errors(&ok));
    let no_pool = format!("{base}[[identities]]\nname = \"pump-planner\"\nkind = \"planner\"\n");
    assert!(
        errors(&no_pool)
            .iter()
            .any(|m| m.contains("must name a pool"))
    );
    let parked = format!(
        "{base}[[identities]]\nname = \"pump-planner\"\nkind = \"planner\"\nstatus = \"parked\"\npool = \"here\"\n"
    );
    assert!(errors(&parked).iter().any(|m| m.contains("must be active")));
}

#[test]
fn a_merge_role_of_the_right_kind_with_a_pool_is_left_alone() {
    let raw = V15.replace(
        "name = \"merge\"\nkind = \"hand\"",
        "name = \"merge\"\nkind = \"merge\"\npool = \"glm\"",
    );
    let plan = plan_of(&raw);
    assert!(plan.kind_changes.is_empty(), "{plan:?}");
    assert!(plan.assignments.iter().all(|row| row.role != "merge"));
    assert!(plan.new_pools.iter().all(|pool| pool.name != "host"));
}

#[test]
fn validate_rejects_a_merge_role_whose_kind_is_not_merge() {
    let raw = V15
        .replace(
            "name = \"merge\"\nkind = \"hand\"",
            "name = \"merge\"\nkind = \"hand\"\npool = \"glm\"",
        )
        .replace(
            "name = \"hand\"\nkind = \"hand\"",
            "name = \"hand\"\nkind = \"hand\"\npool = \"glm\"",
        )
        .replace(
            "[pools.glm]",
            "[[identities]]\nname = \"pump-planner\"\nkind = \"planner\"\nstatus = \"active\"\npool = \"glm\"\nmodel = \"P2-S0\"\n\n[pools.glm]",
        )
        .replace(
            "name = \"auditor\"\nkind = \"auditor\"",
            "name = \"auditor\"\nkind = \"auditor\"\npool = \"glm\"",
        );
    let config: MailspaceConfig = toml::from_str(&raw).unwrap();
    let issues = validate(&config);
    let error = issues
        .iter()
        .find(|issue| issue.severity == Severity::Error && issue.message.contains("kind hand"));
    let error = error.unwrap_or_else(|| panic!("{issues:?}"));
    assert!(error.message.contains("vivi config migrate"), "{error:?}");
    assert!(error.message.contains("kind = \"merge\""), "{error:?}");

    let fixed = raw.replace(
        "name = \"merge\"\nkind = \"hand\"",
        "name = \"merge\"\nkind = \"merge\"",
    );
    let config: MailspaceConfig = toml::from_str(&fixed).unwrap();
    assert!(
        validate(&config)
            .iter()
            .all(|issue| issue.severity != Severity::Error),
        "{:?}",
        validate(&config)
    );
}

#[test]
fn retired_keys_are_removed_with_their_reasons_and_comments_survive() {
    let (plan, after) = migrate(V15);
    let removed: Vec<(&str, &str)> = plan
        .removals
        .iter()
        .map(|row| (row.table.as_str(), row.key.as_str()))
        .collect();
    for expected in [
        ("pump.planner", "run"),
        ("pump.planner", "charter"),
        ("pump.gate", "every_merges"),
        ("pump.gate", "every_seconds"),
        ("pump.merge", "lanes"),
        ("pump", "priority_file"),
    ] {
        assert!(removed.contains(&expected), "{expected:?} in {removed:?}");
    }
    assert!(plan.removals.iter().all(|row| !row.reason.is_empty()));
    for row in &plan.removals {
        assert_eq!(row.behaviour_change, row.table == "pump.gate", "{row:?}");
    }
    for gone in [
        "run = \"pi -p\"",
        "charter =",
        "every_merges",
        "every_seconds",
        "lanes =",
        "priority_file",
    ] {
        assert!(!after.contains(gone), "{gone} still in:\n{after}");
    }
    for kept in [
        "# keep-me",
        "split_hard_cap = 1500  # keep-inline",
        "batch = 4",
        "command = \"cargo test\"",
    ] {
        assert!(after.contains(kept), "{kept} missing from:\n{after}");
    }
    let text = render_plan(&plan);
    assert!(
        text.contains("!! BEHAVIOUR CHANGE: remove [pump.gate] every_merges = 8"),
        "{text}"
    );
    assert!(text.contains("remove [pump.merge] lanes = 3"), "{text}");
    assert!(
        text.contains("remove [pump.planner] run = \"pi -p\""),
        "{text}"
    );
}

#[test]
fn the_retired_board_intake_keys_are_removed_naming_the_pump_record() {
    let raw = V15.replace(
        "[pump.dispatch]\ndefault_kind",
        "[pump.board]\nfrom = [\"operator@x\"]\nsince = \"2026-10-14T00:10\"\nmark = \"pump\"\n\n[pump.dispatch]\ndefault_kind",
    );
    let (plan, after) = migrate(&raw);
    for key in ["mark", "since"] {
        let row = plan
            .removals
            .iter()
            .find(|row| row.table == "pump.board" && row.key == key)
            .unwrap_or_else(|| panic!("{key} is removed: {:?}", plan.removals));
        assert!(row.behaviour_change, "{row:?}");
        assert!(row.reason.contains("vivi pump send --need"), "{row:?}");
    }
    assert!(after.contains("from = [\"operator@x\"]"), "{after}");
    assert!(!after.contains("mark ="), "{after}");
    assert!(!after.contains("since ="), "{after}");
    let text = render_plan(&plan);
    assert!(
        text.contains("!! BEHAVIOUR CHANGE: remove [pump.board] mark = \"pump\""),
        "{text}"
    );
}

#[test]
fn migrated_text_still_parses_as_a_pump_config() {
    let (_, after) = migrate(V15);
    let migrated: MailspaceConfig = toml::from_str(&after).unwrap();
    let pump = migrated.pump.expect("pump");
    assert_eq!(pump.planner.split_hard_cap, 1500);
    assert_eq!(pump.merge.batch, 4);
    assert_eq!(pump.merge.lanes, 1);
    assert_eq!(pump.gate.command, "cargo test");
    let errors: Vec<_> = validate(&toml::from_str::<MailspaceConfig>(&after).unwrap())
        .into_iter()
        .filter(|issue| issue.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn a_second_migrate_of_the_written_text_changes_nothing() {
    let mut with_merge_charter = legacy("planner charter\n");
    with_merge_charter
        .charters
        .insert("merge".into(), "Your own merge rules.\n".into());
    for (raw, first) in [
        (V15.to_string(), with_merge_charter),
        (without_id(V15), MigrateInputs::default()),
    ] {
        let (plan, after) = migrate_with(&raw, &first);
        assert_eq!(plan.charters.len(), 2);
        let second = MigrateInputs {
            legacy_planner_charter: None,
            ..written(&first, &plan)
        };
        let (again, after_again) = migrate_with(&after, &second);
        assert_eq!(after, after_again);
        assert!(
            again.removals.is_empty()
                && again.assignments.is_empty()
                && again.new_roles.is_empty()
                && again.new_pools.is_empty()
                && again.kind_changes.is_empty()
                && again.roots.is_empty()
                && again.charters.is_empty()
                && again.review.is_empty(),
            "{again:?}"
        );
        assert_eq!(render_plan(&again), "already using role.pool assignments");
    }
}

#[test]
fn local_pools_keep_the_15_root_when_the_mailspace_has_no_id() {
    let raw = without_id(V15)
        .replace(
            "[pools.glm]",
            "[pools.fresh]\nprovider = \"local\"\nslots = 1\n\n[pools.kept]\nprovider = \"local\"\nslots = 1\n\n[pools.kept.local]\nroot = \"~/elsewhere\"\n\n[pools.remote]\nprovider = \"fly\"\nslots = 1\n\n[pools.glm]",
        );
    let (plan, after) = migrate(&raw);
    let named: Vec<&str> = plan.roots.iter().map(|row| row.pool.as_str()).collect();
    assert_eq!(named, ["fresh", "glm", "host"], "{plan:?}");
    assert!(plan.roots.iter().all(|row| row.root == "~/vivi-pool"));
    let migrated: MailspaceConfig = toml::from_str(&after).unwrap();
    let root = |pool: &str| migrated.pools[pool].local.root.clone();
    assert_eq!(root("glm").as_deref(), Some("~/vivi-pool"));
    assert_eq!(root("fresh").as_deref(), Some("~/vivi-pool"));
    assert_eq!(root("kept").as_deref(), Some("~/elsewhere"));
    assert_eq!(root("host").as_deref(), Some("~/vivi-pool"));
    assert_eq!(root("remote"), None);
    assert_eq!(
        migrated.pools["glm"].local.worktree_of.as_deref(),
        Some("~/work/t")
    );
    assert!(plan.review.iter().all(|item| !item.contains("not reused")));
}

#[test]
fn the_explicit_root_is_the_directory_15_used_only_without_a_mailspace_id() {
    let project = Path::new("/project");
    let v15 = |slug: &str| {
        crate::pool::local_slots::expand_root("~/vivi-pool")
            .join(slug)
            .join("glm")
    };
    let slug = crate::pool::slot::space_key("Faber Lang", "");
    let site = crate::pool::local_site::resolve("~/vivi-pool", project, &slug, "glm").unwrap();
    assert_eq!(site.pool_dir, v15("faber-lang"));

    let id = crate::pool::slot::space_key("Faber Lang", "11111111-2222-3333-4444-555555555555");
    let site = crate::pool::local_site::resolve("~/vivi-pool", project, &id, "glm").unwrap();
    assert_ne!(site.pool_dir, v15("faber-lang"));

    let default = crate::pool::local_site::resolve(
        crate::pool::config::DEFAULT_LOCAL_ROOT,
        project,
        &id,
        "glm",
    )
    .unwrap();
    assert_eq!(default.pool_dir, project.join(".vivi/pool/glm"));
}

#[test]
fn local_pools_of_a_mailspace_with_an_id_get_a_review_item_not_a_root() {
    let (plan, after) = migrate(V15);
    assert!(plan.roots.is_empty(), "{plan:?}");
    assert!(!after.contains("vivi-pool"), "{after}");
    let item = plan
        .review
        .iter()
        .find(|item| item.contains("not reused"))
        .unwrap_or_else(|| panic!("{plan:?}"));
    assert!(item.contains("glm"), "{item}");
    assert!(item.contains("~/vivi-pool/t/"), "{item}");
}

#[test]
fn render_ends_with_the_items_that_need_a_human() {
    let text = render_plan(&plan_of(V15));
    let lines: Vec<&str> = text.lines().collect();
    let first_review = lines
        .iter()
        .position(|line| line.starts_with("review: "))
        .expect("review lines");
    assert!(
        lines[first_review..]
            .iter()
            .all(|line| line.starts_with("review: "))
    );
    assert!(
        lines[..first_review]
            .iter()
            .any(|line| line.starts_with("remove "))
    );
    assert!(
        lines[..first_review]
            .iter()
            .any(|line| line.starts_with("set role merge kind=merge"))
    );
}

#[test]
fn an_auditor_without_a_pool_gets_the_hand_pool_and_the_shipped_charter() {
    let (plan, after) = migrate(V15);
    assert!(
        plan.assignments
            .iter()
            .any(|row| row.role == "auditor" && row.pool == "glm"),
        "{plan:?}"
    );
    let writes = writes_for(&plan, "auditor");
    assert_eq!(writes.len(), 1, "{plan:?}");
    assert_eq!(writes[0].text, AUDITOR_CHARTER);
    let migrated: MailspaceConfig = toml::from_str(&after).unwrap();
    assert_eq!(role_of(&migrated, "auditor").pool.as_deref(), Some("glm"));
}

#[test]
fn a_missing_auditor_is_added_read_only_on_the_hand_pool() {
    let raw = V15.replace(
        "[[identities]]\nname = \"auditor\"\nkind = \"auditor\"\nstatus = \"active\"\nmodel = \"P2-S0\"\n\n",
        "",
    );
    let (plan, after) = migrate(&raw);
    let added = plan.new_roles.iter().find(|role| role.name == "auditor");
    let added = added.expect("auditor added");
    assert_eq!(
        (added.kind.as_str(), added.pool.as_str()),
        ("auditor", "glm")
    );
    assert_eq!(added.read_only, Some(true));
    assert_eq!(writes_for(&plan, "auditor").len(), 1);
    let migrated: MailspaceConfig = toml::from_str(&after).unwrap();
    let auditor = role_of(&migrated, "auditor");
    assert_eq!(auditor.status, "active");
    assert_eq!(auditor.read_only, Some(true));
}

#[test]
fn an_auditor_charter_that_has_text_is_kept_and_reported() {
    let mut inputs = MigrateInputs::default();
    inputs.charters.insert(
        "auditor".into(),
        "You are a Tugboat auditor seat.\nReport to the Mind.\n".into(),
    );
    let plan = plan_with(V15, &inputs);
    assert!(writes_for(&plan, "auditor").is_empty(), "{plan:?}");
    let item = plan
        .review
        .iter()
        .find(|item| item.starts_with("auditor role auditor has its own charter"))
        .unwrap_or_else(|| panic!("{plan:?}"));
    assert!(item.contains("need acceptance audits"), "{item}");
    assert!(item.contains("You are a Tugboat auditor seat."), "{item}");
}

#[test]
fn an_auditor_with_a_pool_is_left_alone() {
    let raw = V15.replace(
        "name = \"auditor\"\nkind = \"auditor\"",
        "name = \"auditor\"\nkind = \"auditor\"\npool = \"glm\"",
    );
    let plan = plan_of(&raw);
    assert!(plan.assignments.iter().all(|row| row.role != "auditor"));
    assert!(plan.new_roles.iter().all(|role| role.name != "auditor"));
}

#[test]
fn validate_requires_an_active_auditor_with_a_pool() {
    let base = "name = \"t\"\n[[identities]]\nname = \"hand\"\nkind = \"hand\"\npool = \"here\"\n[[identities]]\nname = \"merge\"\nkind = \"merge\"\npool = \"here\"\n[[identities]]\nname = \"pump-planner\"\nkind = \"planner\"\npool = \"here\"\n[[identities]]\nname = \"researcher\"\nkind = \"researcher\"\npool = \"here\"\n[[identities]]\nname = \"admitter\"\nkind = \"admitter\"\npool = \"here\"\n[[identities]]\nname = \"slicer\"\nkind = \"slicer\"\npool = \"here\"\n[pools.here]\nprovider = \"local\"\nslots = 1\n[pump]\nbranch = \"next\"\nto = \"hand\"\n";
    // Everything that stops the pump: wrong values and missing items.
    let errors = |text: &str| -> Vec<String> {
        let config: MailspaceConfig = toml::from_str(&runnable(text)).unwrap();
        validate(&config)
            .into_iter()
            .filter(|issue| issue.severity != Severity::Warning)
            .map(|issue| issue.message)
            .collect()
    };
    let none = errors(base);
    assert_eq!(none.len(), 1, "{none:?}");
    assert!(
        none[0].contains("auditor") && none[0].contains("vivi config migrate"),
        "{none:?}"
    );
    let no_pool = errors(&format!(
        "{base}[[identities]]\nname = \"auditor\"\nkind = \"auditor\"\n"
    ));
    assert!(
        no_pool
            .iter()
            .any(|m| m.contains("must name a pool") && m.contains("vivi config migrate")),
        "{no_pool:?}"
    );
    let parked = errors(&format!(
        "{base}[[identities]]\nname = \"auditor\"\nkind = \"auditor\"\nstatus = \"parked\"\npool = \"here\"\n"
    ));
    assert_eq!(parked.len(), 1, "{parked:?}");
    let ok = errors(&format!(
        "{base}[[identities]]\nname = \"auditor\"\nkind = \"auditor\"\npool = \"here\"\n"
    ));
    assert!(ok.is_empty(), "{ok:?}");
    let by_kind = errors(&format!(
        "{base}[[identities]]\nname = \"checker\"\nkind = \"auditor\"\npool = \"here\"\n"
    ));
    assert!(by_kind.is_empty(), "{by_kind:?}");
}

#[test]
fn a_hand_role_without_a_kind_is_set_to_hand() {
    let raw = V15.replace("name = \"hand\"\nkind = \"hand\"\n", "name = \"hand\"\n");
    let (plan, after) = migrate(&raw);
    let change = plan
        .kind_changes
        .iter()
        .find(|change| change.role == "hand");
    let change = change.unwrap_or_else(|| panic!("{plan:?}"));
    assert_eq!((change.from.as_deref(), change.to.as_str()), (None, "hand"));
    let migrated: MailspaceConfig = toml::from_str(&after).unwrap();
    assert_eq!(role_of(&migrated, "hand").kind.as_deref(), Some("hand"));
}

#[test]
fn a_hand_role_of_another_kind_is_reported_not_changed() {
    let raw = V15.replace(
        "name = \"hand\"\nkind = \"hand\"\n",
        "name = \"hand\"\nkind = \"mind\"\n",
    );
    let (plan, after) = migrate(&raw);
    assert!(plan.kind_changes.iter().all(|change| change.role != "hand"));
    let item = plan
        .review
        .iter()
        .find(|item| item.starts_with("pump role hand has kind mind"))
        .unwrap_or_else(|| panic!("{plan:?}"));
    assert!(item.contains("--kind hand"), "{item}");
    let migrated: MailspaceConfig = toml::from_str(&after).unwrap();
    assert_eq!(role_of(&migrated, "hand").kind.as_deref(), Some("mind"));
}

#[test]
fn validate_requires_the_pump_hand_to_be_kind_hand() {
    let text = |kind: &str| {
        format!(
            "name = \"t\"\n[[identities]]\nname = \"hand\"\n{kind}pool = \"here\"\n[[identities]]\nname = \"merge\"\nkind = \"merge\"\npool = \"here\"\n[[identities]]\nname = \"pump-planner\"\nkind = \"planner\"\npool = \"here\"\n[[identities]]\nname = \"auditor\"\nkind = \"auditor\"\npool = \"here\"\n[pools.here]\nprovider = \"local\"\nslots = 1\n[pump]\nbranch = \"next\"\nto = \"hand\"\n"
        )
    };
    let errors = |text: &str| -> Vec<String> {
        let config: MailspaceConfig = toml::from_str(&runnable(text)).unwrap();
        validate(&config)
            .into_iter()
            .filter(|issue| issue.severity == Severity::Error)
            .map(|issue| issue.message)
            .collect()
    };
    for kind in ["", "kind = \"mind\"\n"] {
        let found = errors(&text(kind));
        assert_eq!(found.len(), 1, "{kind:?} {found:?}");
        assert!(found[0].contains("not authorized to publish"), "{found:?}");
        assert!(found[0].contains("vivi config migrate"), "{found:?}");
    }
    assert!(errors(&text("kind = \"hand\"\n")).is_empty());
}

#[test]
fn the_pump_role_checks_name_each_requirement_for_doctor() {
    let config: MailspaceConfig = toml::from_str(V15).unwrap();
    let checks = crate::routing::pump_role_checks(&config);
    let failing: Vec<&str> = checks
        .iter()
        .filter(|check| !check.issues.is_empty())
        .map(|check| check.name)
        .collect();
    assert_eq!(
        failing,
        [
            "pump-planner",
            "researcher",
            "admitter",
            "slicer",
            "merge",
            "merge kind",
            "auditor"
        ]
    );
    let none: MailspaceConfig = toml::from_str("name = \"t\"\n").unwrap();
    assert!(crate::routing::pump_role_checks(&none).is_empty());
}

#[test]
fn the_new_host_pool_copies_the_hand_pool_so_it_can_run_jobs() {
    let (plan, after) = migrate(V15);
    let host = plan.new_pools.iter().find(|pool| pool.name == "host");
    let host = host.unwrap_or_else(|| panic!("{plan:?}"));
    assert_eq!(host.copy_from.as_deref(), Some("glm"));
    assert_eq!(host.slots, 3, "slots come from [pump.merge] lanes");
    let text = render_plan(&plan);
    assert!(text.contains("settings copied from glm"), "{text}");

    let migrated: MailspaceConfig = toml::from_str(&after).unwrap();
    let (glm, host) = (&migrated.pools["glm"], &migrated.pools["host"]);
    assert_eq!(host.slots, 3);
    assert_eq!(host.provider, "local");
    assert_eq!(host.build_dir, glm.build_dir);
    assert_eq!(host.worktree_source(), glm.worktree_source());
    assert!(host.worktree_source().is_some(), "{after}");
    let (host_run, glm_run) = (
        host.harness.as_ref().unwrap(),
        glm.harness.as_ref().unwrap(),
    );
    assert_eq!(host_run.run, glm_run.run);
    assert_eq!(host_run.models, glm_run.models);
    assert_eq!(host_run.env, glm_run.env);
    assert_eq!(host.committer.as_ref().unwrap().email, "bot@example.com");
    assert_eq!(host.bands.entries.len(), glm.bands.entries.len());
    assert!(!host.bands.is_empty());
    assert_eq!(glm.slots, 10, "the hand pool is left alone");
    assert!(
        plan.review
            .iter()
            .any(|item| item.contains("pool host: merge lanes want persistent warm slots")),
        "{plan:?}"
    );
}

#[test]
fn a_hand_pool_on_another_provider_is_not_copied() {
    let raw = V15.replace(
        "[pools.glm]\nprovider = \"local\"",
        "[pools.glm]\nprovider = \"fly\"",
    );
    let plan = plan_of(&raw);
    let host = plan.new_pools.iter().find(|pool| pool.name == "host");
    assert_eq!(host.expect("host pool").copy_from, None, "{plan:?}");
    assert!(
        plan.review
            .iter()
            .any(|item| item.contains("give it a source")),
        "{plan:?}"
    );
}

#[test]
fn new_pump_roles_start_at_the_hand_band() {
    let (plan, after) = migrate(V15);
    let planner = plan
        .new_roles
        .iter()
        .find(|role| role.name == "pump-planner")
        .expect("pump-planner added");
    assert_eq!(planner.model.as_deref(), Some("P2-S0"));
    let migrated: MailspaceConfig = toml::from_str(&after).unwrap();
    assert_eq!(
        role_of(&migrated, "pump-planner").model.as_deref(),
        Some("P2-S0")
    );
    let errors: Vec<_> = validate(&migrated)
        .into_iter()
        .filter(|issue| issue.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}
