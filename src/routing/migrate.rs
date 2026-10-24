//! Convert old-style pump routing into `role.pool` assignments.

use std::collections::BTreeMap;

use toml_edit::{DocumentMut, Item, Table};

use crate::error::VivariumError;
use crate::mailspace::config_edit::{IdentityPatch, insert_identity, patch_identity};
use crate::mailspace::shipped::{
    ADMITTER_ROLE, AUDITOR_CHARTER, MERGE_CHARTER, PUMP_PLANNER_CHARTER, PUMP_PLANNER_ROLE,
    RESEARCHER_ROLE, SLICER_ROLE, pre_v20_charter,
};
use crate::mailspace::{LocalIdentity, MailspaceConfig};
use crate::pump::config::PumpConfig;

use super::defaults::is_band_model;
use super::migrate_charters::plan_stale_charters;
use super::migrate_flow_roles::plan_flow_roles;
use super::migrate_keys::{
    apply_removals, apply_roots, plan_pool_roots, plan_removals, raw_merge_lanes, review_slot_dirs,
};
use super::migrate_pool::{add_pool, settings_source};

/// The flow roles take no band from the hand role.
const FLOW_ROLES: [&str; 3] = [RESEARCHER_ROLE, ADMITTER_ROLE, SLICER_ROLE];

/// One planned file change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigratePlan {
    pub assignments: Vec<Assignment>,
    pub new_roles: Vec<NewRole>,
    pub new_pools: Vec<NewPool>,
    pub caps: Vec<CapWrite>,
    /// Roles whose `kind` changes.
    pub kind_changes: Vec<KindChange>,
    /// Keys the plan deletes because the current release ignores them.
    pub removals: Vec<KeyRemoval>,
    /// Local pools that get an explicit `root`.
    pub roots: Vec<RootSet>,
    /// Role charters the plan writes (only under `--write`).
    pub charters: Vec<CharterWrite>,
    pub notes: Vec<String>,
    /// Items the plan cannot settle: a human has to look at them.
    pub review: Vec<String>,
}

/// What migrate reads from outside the document.
#[derive(Debug, Clone, Default)]
pub struct MigrateInputs {
    /// Each role's current charter text, by role name. A role absent here
    /// has no charter.
    pub charters: BTreeMap<String, String>,
    /// The file `[pump.planner] charter` names, when it is readable and not
    /// blank.
    pub legacy_planner_charter: Option<LegacyCharter>,
}

/// A readable `[pump.planner] charter` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyCharter {
    /// The path as written in the file.
    pub path: String,
    pub text: String,
}

/// One role charter the plan writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharterWrite {
    pub role: String,
    /// Where the text comes from, for the plan.
    pub origin: String,
    pub text: String,
}

/// One role whose `kind` the plan sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindChange {
    pub role: String,
    /// The kind now in the file, if any.
    pub from: Option<String>,
    pub to: String,
    pub reason: String,
}

/// One config key the plan deletes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRemoval {
    /// The dotted table that holds the key (`pump.gate`).
    pub table: String,
    pub key: String,
    /// The key's current value, as written in the file.
    pub value: String,
    pub reason: String,
    /// The removal changes how the pump behaves, not just the file.
    pub behaviour_change: bool,
}

/// `[pools.<pool>.local] root = ...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootSet {
    pub pool: String,
    pub root: String,
    pub reason: String,
}

/// `role.pool = name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub role: String,
    pub pool: String,
    pub reason: String,
}

/// A role the plan will add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRole {
    pub name: String,
    pub kind: String,
    pub pool: String,
    /// Written only when set; absent leaves the kind default.
    pub read_only: Option<bool>,
    /// The hand role's band, when it has one: pump jobs are sent without a
    /// model, so a role with no band cannot be dispatched.
    pub model: Option<String>,
}

/// A pool the plan will add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPool {
    pub name: String,
    pub provider: String,
    pub slots: u32,
    pub reason: String,
    /// The pool whose provider, source, harness, committer, build directory
    /// and bands the new pool copies: the hand role's pool.
    pub copy_from: Option<String>,
}

/// One `[pools.X.caps]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapWrite {
    pub pool: String,
    pub model: String,
    pub cap: u32,
}

/// What migrate produced, including the edited document text.
#[derive(Debug, Clone)]
pub struct MigrateOutcome {
    pub plan: MigratePlan,
    pub before: String,
    pub after: String,
}

/// Build a plan from a loaded config and the document it came from. Does not
/// write. The document tells which keys are present: the config structs fill
/// in defaults for absent ones.
#[must_use]
pub fn plan_migrate(
    config: &MailspaceConfig,
    doc: &DocumentMut,
    inputs: &MigrateInputs,
) -> MigratePlan {
    let mut plan = MigratePlan::default();
    if let Some(pump) = &config.pump {
        plan_pump(config, pump, doc, inputs, &mut plan);
    }
    plan_pool_roots(config, &mut plan);
    if config.pump.is_none() && plan_is_empty(&plan) {
        plan.notes
            .push("no [pump] table; nothing to migrate".into());
    } else if plan_is_empty(&plan) {
        plan.notes
            .push("already using role.pool assignments".into());
    } else {
        // Advice about the file as it stands: given only while this run
        // changes something, so a migrated file stays quiet.
        review_slot_dirs(config, &mut plan);
        if let Some(pump) = &config.pump {
            review_own_charters(config, inputs, &mut plan);
            review_hand_kind(config, pump, &mut plan);
        }
    }
    plan
}

fn plan_pump(
    config: &MailspaceConfig,
    pump: &PumpConfig,
    doc: &DocumentMut,
    inputs: &MigrateInputs,
    plan: &mut MigratePlan,
) {
    let dispatch = dispatch_pool(pump);
    assign_hand(config, pump, dispatch.as_deref(), plan);
    let lanes = raw_merge_lanes(doc);
    let merge_pool = assign_merge(config, pump, lanes, plan);
    plan_pump_planner(config, pump, inputs, plan);
    plan_auditor(config, pump, inputs, plan);
    plan_hand_kind(config, pump, plan);
    assign_slug_roles(config, plan);
    fold_caps(config, pump, dispatch.as_deref(), plan);
    plan_removals(doc, config, &merge_pool, plan);
    plan_flow_roles(config, pump, inputs, plan);
    inherit_hand_band(config, pump, plan);
    review_new_roles(plan);
    plan_stale_charters(config, inputs, plan);
}

/// New pump roles start at the hand role's band, so the pool serves them as
/// it serves the hand. A hand with no band leaves them without one. The flow
/// roles take no band from the hand: each one gets a review line asking the
/// operator to set its own.
fn inherit_hand_band(config: &MailspaceConfig, pump: &PumpConfig, plan: &mut MigratePlan) {
    let band = find_role(config, &pump.to)
        .and_then(|role| role.model.clone())
        .filter(|model| is_band_model(model));
    for role in &mut plan.new_roles {
        if FLOW_ROLES.contains(&role.name.as_str()) {
            plan.review.push(format!(
                "set a band for {0}: `vivi role set {0} --model <band>`",
                role.name
            ));
        } else {
            role.model.clone_from(&band);
        }
    }
}

fn plan_is_empty(plan: &MigratePlan) -> bool {
    plan.assignments.is_empty()
        && plan.new_roles.is_empty()
        && plan.new_pools.is_empty()
        && plan.caps.is_empty()
        && plan.kind_changes.is_empty()
        && plan.removals.is_empty()
        && plan.roots.is_empty()
        && plan.charters.is_empty()
        && plan.review.is_empty()
}

/// A role the plan adds has no charter file yet.
fn review_new_roles(plan: &mut MigratePlan) {
    let names: Vec<&str> = plan
        .new_roles
        .iter()
        .filter(|role| !matches!(role.name.as_str(), "gate" | "auditor" | PUMP_PLANNER_ROLE))
        .filter(|role| !plan.charters.iter().any(|write| write.role == role.name))
        .map(|role| role.name.as_str())
        .collect();
    if names.is_empty() {
        return;
    }
    plan.review.push(format!(
        "new role(s) {} have no charter: add one with `vivi role charter set <role> --file <path>`",
        names.join(", ")
    ));
}

fn dispatch_pool(pump: &PumpConfig) -> Option<String> {
    let default = pump.dispatch.default_kind.as_str();
    pump.dispatch
        .kinds
        .get(default)
        .and_then(|kind| kind.pools.first())
        .cloned()
        .or_else(|| {
            pump.dispatch
                .kinds
                .values()
                .find_map(|kind| kind.pools.first().cloned())
        })
}

fn assign_hand(
    config: &MailspaceConfig,
    pump: &PumpConfig,
    dispatch: Option<&str>,
    plan: &mut MigratePlan,
) {
    let Some(pool) = dispatch else {
        return;
    };
    let Some(role) = find_role(config, &pump.to) else {
        return;
    };
    if role.pool.is_some() {
        return;
    }
    plan.assignments.push(Assignment {
        role: role.name.clone(),
        pool: pool.to_string(),
        reason: format!("[pump.dispatch.kinds.{}] pools", pump.dispatch.default_kind),
    });
}

/// Give the merge role a pool and the `merge` kind, adding the role when the
/// mailspace has none. Returns the pool the merge role ends up with.
fn assign_merge(
    config: &MailspaceConfig,
    pump: &PumpConfig,
    lanes: Option<u32>,
    plan: &mut MigratePlan,
) -> String {
    let Some(role) = find_role(config, "merge") else {
        let pool = choose_merge_pool(config, pump, lanes, plan);
        plan.new_roles.push(NewRole {
            name: "merge".into(),
            kind: "merge".into(),
            pool: pool.clone(),
            read_only: None,
            model: None,
        });
        return pool;
    };
    if !has_kind(role, "merge") {
        plan.kind_changes.push(KindChange {
            role: role.name.clone(),
            from: role.kind.clone(),
            to: "merge".into(),
            reason: "only a merge-kind role may publish to main".into(),
        });
    }
    if let Some(pool) = &role.pool {
        return pool.clone();
    }
    let pool = choose_merge_pool(config, pump, lanes, plan);
    plan.assignments.push(Assignment {
        role: role.name.clone(),
        pool: pool.clone(),
        reason: "the merge role needs a pool".into(),
    });
    pool
}

fn has_kind(role: &LocalIdentity, kind: &str) -> bool {
    role.kind
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case(kind))
}

/// The pool a merge role without one gets: the pump-host pool, added when the
/// mailspace lacks it.
fn choose_merge_pool(
    config: &MailspaceConfig,
    pump: &PumpConfig,
    lanes: Option<u32>,
    plan: &mut MigratePlan,
) -> String {
    let name = host_pool_name(config);
    if !config.pools.contains_key(&name) && !plan.new_pools.iter().any(|pool| pool.name == name) {
        let copy_from = settings_source(config, pump, plan);
        let start = match &copy_from {
            Some(from) => format!("its source and harness are copied from pool {from}; seed it"),
            None => format!(
                "give it a source (`[pools.{name}.local] worktree_of` or `repo`) and a \
                 harness, and seed it"
            ),
        };
        plan.new_pools.push(NewPool {
            name: name.clone(),
            provider: "local".into(),
            slots: lanes.unwrap_or(1).max(1),
            reason: "pump-host merge".into(),
            copy_from,
        });
        plan.review.push(format!(
            "pool {name}: merge lanes want persistent warm slots; the new pool starts cold, \
             {start}"
        ));
    }
    name
}

/// Make sure the pump has its own planner role, `pump-planner`, with a pool
/// and a charter. A role named `planner` (a Tugboat seat) is left alone.
fn plan_pump_planner(
    config: &MailspaceConfig,
    pump: &PumpConfig,
    inputs: &MigrateInputs,
    plan: &mut MigratePlan,
) {
    let existing = config
        .identities
        .iter()
        .find(|role| role.name.eq_ignore_ascii_case(PUMP_PLANNER_ROLE));
    if existing.is_some_and(|role| role.status != "active") {
        return;
    }
    let pool = hand_pool(config, pump, plan);
    match (existing, pool) {
        (Some(role), _) if role.pool.is_some() => {}
        (Some(role), Some(pool)) => plan.assignments.push(Assignment {
            role: role.name.clone(),
            pool,
            reason: "the pool of the hand role".into(),
        }),
        (None, Some(pool)) => plan.new_roles.push(NewRole {
            name: PUMP_PLANNER_ROLE.into(),
            kind: "planner".into(),
            pool,
            read_only: None,
            model: None,
        }),
        (_, None) => {
            plan.review.push(format!(
                "{PUMP_PLANNER_ROLE} needs a pool: run vivi role pool {PUMP_PLANNER_ROLE} <pool> \
                 (add the role first with `vivi role add {PUMP_PLANNER_ROLE} --kind planner` if it is missing)"
            ));
            return;
        }
    }
    let legacy = pump.planner.charter.trim();
    if !legacy.is_empty() && inputs.legacy_planner_charter.is_none() {
        plan.review.push(format!(
            "[pump.planner] charter {legacy} is not a readable, non-blank file; {PUMP_PLANNER_ROLE} \
             gets the shipped default charter"
        ));
    }
    plan_pump_planner_charter(existing.map(|role| role.name.as_str()), inputs, plan);
}

/// Give a `pump-planner` with a blank charter the charter of the old
/// `[pump.planner] charter` file, else the shipped default.
fn plan_pump_planner_charter(
    existing: Option<&str>,
    inputs: &MigrateInputs,
    plan: &mut MigratePlan,
) {
    let current = existing.and_then(|name| inputs.charters.get(name));
    if current.is_some_and(|text| !text.trim().is_empty()) {
        return;
    }
    let shipped = || {
        (
            "the shipped default".to_string(),
            PUMP_PLANNER_CHARTER.to_string(),
        )
    };
    let (origin, text) = match &inputs.legacy_planner_charter {
        Some(legacy) if pre_v20_charter(&legacy.text) => {
            plan.review.push(format!(
                "{} is ignored: it has no `+++` v20 reply envelope; {PUMP_PLANNER_ROLE} gets the \
                 shipped default charter",
                legacy.path
            ));
            shipped()
        }
        Some(legacy) => {
            plan.review.push(format!(
                "{PUMP_PLANNER_ROLE} charter was copied from {}, which was written for 15: check \
                 that it ends with the `+++` TOML reply envelope the pump planner returns (the \
                 pool contract is added to every prompt), or use the shipped charter \
                 (`vivi skills show vivi --file references/charters/pump-planner.md`)",
                legacy.path
            ));
            (legacy.path.clone(), legacy.text.clone())
        }
        None => shipped(),
    };
    plan.charters.push(CharterWrite {
        role: PUMP_PLANNER_ROLE.into(),
        origin,
        text,
    });
}

/// The auditor role: need acceptance audits go to it and have no fallback, so
/// it needs a pool and a charter. A missing one is added read-only.
fn plan_auditor(
    config: &MailspaceConfig,
    pump: &PumpConfig,
    inputs: &MigrateInputs,
    plan: &mut MigratePlan,
) {
    let existing = find_role(config, "auditor");
    if existing.is_some_and(|role| role.status != "active") {
        return;
    }
    let Some(pool) = hand_pool(config, pump, plan) else {
        plan.review.push(
            "auditor needs a pool: run vivi role pool auditor <pool> (add the role first with \
             `vivi role add auditor --kind auditor --read-only true` if it is missing)"
                .into(),
        );
        return;
    };
    match existing {
        Some(role) if role.pool.is_some() => {}
        Some(role) => plan.assignments.push(Assignment {
            role: role.name.clone(),
            pool,
            reason: "need acceptance audits need an auditor with a pool; the pool of the hand role"
                .into(),
        }),
        None => plan.new_roles.push(NewRole {
            name: "auditor".into(),
            kind: "auditor".into(),
            pool,
            read_only: Some(true),
            model: None,
        }),
    }
    let name = existing.map_or("auditor", |role| role.name.as_str());
    let blank = inputs
        .charters
        .get(name)
        .is_none_or(|text| text.trim().is_empty());
    if blank {
        plan.charters.push(CharterWrite {
            role: name.into(),
            origin: "the shipped auditor charter".into(),
            text: AUDITOR_CHARTER.into(),
        });
    }
}

/// The pump's `to` role must be a hand to publish unit branches. A missing
/// kind is set; another kind is the operator's call.
fn plan_hand_kind(config: &MailspaceConfig, pump: &PumpConfig, plan: &mut MigratePlan) {
    let Some(role) = find_role(config, &pump.to) else {
        return;
    };
    if role.kind.is_none() {
        plan.kind_changes.push(KindChange {
            role: role.name.clone(),
            from: None,
            to: "hand".into(),
            reason: "only a hand-kind role may publish unit branches".into(),
        });
    }
}

/// The pump's `to` role has a kind other than `hand`: report, never change.
fn review_hand_kind(config: &MailspaceConfig, pump: &PumpConfig, plan: &mut MigratePlan) {
    let Some(role) = find_role(config, &pump.to) else {
        return;
    };
    let Some(kind) = role.kind.as_deref() else {
        return;
    };
    if !kind.trim().eq_ignore_ascii_case("hand") {
        plan.review.push(format!(
            "pump role {} has kind {kind}; only a hand-kind role may publish unit branches, so \
             every unit fails with \"not authorized to publish\". It is not changed here: if it \
             should be a hand, run `vivi role set {} --kind hand`",
            role.name, role.name
        ));
    }
}

/// Roles that keep a charter of their own, whose text is now a system prompt.
fn review_own_charters(config: &MailspaceConfig, inputs: &MigrateInputs, plan: &mut MigratePlan) {
    for (selector, shipped, use_) in [
        ("merge", MERGE_CHARTER, "16 merge jobs"),
        ("auditor", AUDITOR_CHARTER, "16 need acceptance audits"),
    ] {
        let Some(role) = find_role(config, selector) else {
            continue;
        };
        let text = inputs
            .charters
            .get(&role.name)
            .map_or("", |text| text.trim());
        if text.is_empty() || text == shipped.trim() {
            continue;
        }
        let first = text.lines().next().unwrap_or_default();
        plan.review.push(format!(
            "{selector} role {} has its own charter (first line: {first:?}); {use_} use it as \
             the system prompt. It is not changed here. To use the shipped one, save \
             `vivi skills show tugboat --file references/charters/{selector}.md` and run \
             `vivi role charter set {} --file <path>`",
            role.name, role.name
        ));
    }
}

/// The pool the hand role has, or is given by this plan.
pub(super) fn hand_pool(
    config: &MailspaceConfig,
    pump: &PumpConfig,
    plan: &MigratePlan,
) -> Option<String> {
    let role = find_role(config, &pump.to)?;
    role.pool.clone().or_else(|| {
        plan.assignments
            .iter()
            .find(|row| row.role == role.name)
            .map(|row| row.pool.clone())
    })
}

fn host_pool_name(config: &MailspaceConfig) -> String {
    ["host", "pump-host", "local-host"]
        .into_iter()
        .find(|candidate| {
            config
                .pools
                .get(*candidate)
                .is_none_or(|pool| pool.provider == "local")
        })
        .unwrap_or("host")
        .to_string()
}

fn assign_slug_roles(config: &MailspaceConfig, plan: &mut MigratePlan) {
    for role in &config.identities {
        if role.pool.is_some() || role.status == "retired" || already_assigned(plan, &role.name) {
            continue;
        }
        let Some(model) = role.model.as_deref() else {
            continue;
        };
        if is_band_model(model) {
            continue;
        }
        let matches = matching_pools(config, model);
        if matches.len() == 1 {
            plan.assignments.push(Assignment {
                role: role.name.clone(),
                pool: matches[0].clone(),
                reason: format!("harness.models contains `{model}`"),
            });
        }
    }
}

fn already_assigned(plan: &MigratePlan, name: &str) -> bool {
    plan.assignments.iter().any(|row| row.role == name)
        || plan.new_roles.iter().any(|row| row.name == name)
}

fn matching_pools(config: &MailspaceConfig, model: &str) -> Vec<String> {
    config
        .pools
        .iter()
        .filter(|(_, pool)| {
            pool.harness
                .as_ref()
                .is_some_and(|harness| harness.models.iter().any(|offered| offered == model))
        })
        .map(|(name, _)| name.clone())
        .collect()
}

fn fold_caps(
    config: &MailspaceConfig,
    pump: &PumpConfig,
    dispatch: Option<&str>,
    plan: &mut MigratePlan,
) {
    let Some(pool) = dispatch else {
        return;
    };
    if config
        .pools
        .get(pool)
        .is_some_and(|entry| !entry.caps.is_empty())
    {
        return;
    }
    for lane in &pump.dispatch.lanes {
        plan.caps.push(CapWrite {
            pool: pool.to_string(),
            model: lane.model.clone(),
            cap: lane.cap,
        });
    }
}

fn find_role<'a>(config: &'a MailspaceConfig, name: &str) -> Option<&'a LocalIdentity> {
    config
        .identities
        .iter()
        .find(|role| role.name.eq_ignore_ascii_case(name))
        .or_else(|| {
            config.identities.iter().find(|role| {
                role.kind
                    .as_deref()
                    .is_some_and(|kind| kind.eq_ignore_ascii_case(name))
            })
        })
}

/// Human-readable plan, one action per line.
#[must_use]
pub fn render_plan(plan: &MigratePlan) -> String {
    let mut lines = Vec::new();
    render_additions(plan, &mut lines);
    render_changes(plan, &mut lines);
    render_removals(plan, &mut lines);
    lines.extend(plan.notes.iter().cloned());
    lines.extend(plan.review.iter().map(|item| format!("review: {item}")));
    if lines.is_empty() {
        lines.push("no changes".into());
    }
    lines.join("\n")
}

fn render_additions(plan: &MigratePlan, lines: &mut Vec<String>) {
    for pool in &plan.new_pools {
        let copied = pool
            .copy_from
            .as_ref()
            .map_or_else(String::new, |from| format!(" settings copied from {from}"));
        lines.push(format!(
            "add [pools.{}] provider={} slots={}{copied}  ({})",
            pool.name, pool.provider, pool.slots, pool.reason
        ));
    }
    for role in &plan.new_roles {
        let read_only = role
            .read_only
            .map_or_else(String::new, |flag| format!(" read_only={flag}"));
        let model = role
            .model
            .as_ref()
            .map_or_else(String::new, |band| format!(" model={band}"));
        lines.push(format!(
            "add role {} kind={} pool={}{model}{read_only}",
            role.name, role.kind, role.pool
        ));
    }
}

fn render_changes(plan: &MigratePlan, lines: &mut Vec<String>) {
    for row in &plan.assignments {
        lines.push(format!(
            "set role {} pool={}  ({})",
            row.role, row.pool, row.reason
        ));
    }
    for change in &plan.kind_changes {
        lines.push(format!(
            "set role {} kind={} (was {})  ({})",
            change.role,
            change.to,
            change.from.as_deref().unwrap_or("none"),
            change.reason
        ));
    }
    for root in &plan.roots {
        lines.push(format!(
            "set [pools.{}.local] root = {:?}  ({})",
            root.pool, root.root, root.reason
        ));
    }
    for write in &plan.charters {
        lines.push(format!(
            "set role {} charter from {} ({} lines)",
            write.role,
            write.origin,
            write.text.lines().count()
        ));
    }
    for cap in &plan.caps {
        lines.push(format!(
            "set [pools.{}.caps] {:?} = {}",
            cap.pool, cap.model, cap.cap
        ));
    }
}

fn render_removals(plan: &MigratePlan, lines: &mut Vec<String>) {
    for removal in &plan.removals {
        let tag = if removal.behaviour_change {
            "!! BEHAVIOUR CHANGE: "
        } else {
            ""
        };
        lines.push(format!(
            "{tag}remove [{}] {} = {}  ({})",
            removal.table, removal.key, removal.value, removal.reason
        ));
    }
}

/// Apply `plan` to `doc`.
pub fn apply_plan_to_document(
    doc: &mut DocumentMut,
    plan: &MigratePlan,
) -> Result<(), VivariumError> {
    add_pools(doc, plan)?;
    add_roles(doc, plan)?;
    assign_roles(doc, plan)?;
    set_kinds(doc, plan)?;
    apply_roots(doc, plan);
    write_caps(doc, plan);
    apply_removals(doc, plan);
    Ok(())
}

fn set_kinds(doc: &mut DocumentMut, plan: &MigratePlan) -> Result<(), VivariumError> {
    for change in &plan.kind_changes {
        patch_identity(
            doc,
            &change.role,
            &IdentityPatch {
                kind: Some(Some(change.to.clone())),
                ..IdentityPatch::default()
            },
        )?;
    }
    Ok(())
}

fn add_pools(doc: &mut DocumentMut, plan: &MigratePlan) -> Result<(), VivariumError> {
    for pool in &plan.new_pools {
        add_pool(doc, pool)?;
    }
    Ok(())
}

fn add_roles(doc: &mut DocumentMut, plan: &MigratePlan) -> Result<(), VivariumError> {
    for role in &plan.new_roles {
        insert_identity(
            doc,
            &role.name,
            &IdentityPatch {
                kind: Some(Some(role.kind.clone())),
                pool: Some(Some(role.pool.clone())),
                read_only: role.read_only.map(Some),
                model: role.model.clone().map(Some),
                status: Some("active".into()),
                ..IdentityPatch::default()
            },
        )?;
    }
    Ok(())
}

fn assign_roles(doc: &mut DocumentMut, plan: &MigratePlan) -> Result<(), VivariumError> {
    for row in &plan.assignments {
        patch_identity(
            doc,
            &row.role,
            &IdentityPatch {
                pool: Some(Some(row.pool.clone())),
                ..IdentityPatch::default()
            },
        )?;
    }
    Ok(())
}

fn write_caps(doc: &mut DocumentMut, plan: &MigratePlan) {
    let mut grouped: BTreeMap<&str, Vec<&CapWrite>> = BTreeMap::new();
    for cap in &plan.caps {
        grouped.entry(cap.pool.as_str()).or_default().push(cap);
    }
    for (pool, caps) in grouped {
        let Some(table) = pool_table(doc, pool) else {
            continue;
        };
        for cap in caps {
            table["caps"][cap.model.as_str()] = toml_edit::value(i64::from(cap.cap));
        }
    }
}

fn pool_table<'a>(doc: &'a mut DocumentMut, name: &str) -> Option<&'a mut Table> {
    doc.get_mut("pools")
        .and_then(Item::as_table_mut)
        .and_then(|pools| pools.get_mut(name))
        .and_then(Item::as_table_mut)
}
