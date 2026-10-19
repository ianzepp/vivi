//! Convert old-style pump routing into `role.pool` assignments.

use std::collections::BTreeMap;

use toml_edit::{DocumentMut, Item, Table};

use crate::mailspace::config_edit::{IdentityPatch, insert_identity, patch_identity};
use crate::mailspace::{LocalIdentity, MailspaceConfig};
use crate::pump::config::PumpConfig;

use super::defaults::is_band_model;

/// One planned file change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigratePlan {
    pub assignments: Vec<Assignment>,
    pub new_roles: Vec<NewRole>,
    pub new_pools: Vec<NewPool>,
    pub caps: Vec<CapWrite>,
    pub renames: Vec<KeyRename>,
    pub notes: Vec<String>,
}

/// One renamed config key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRename {
    /// The dotted or bracketed table that holds the key.
    pub table: String,
    /// The deprecated key name.
    pub from: String,
    /// The key name that replaces it.
    pub to: String,
    /// Why the rename happens.
    pub note: String,
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
    pub read_only: bool,
}

/// A pool the plan will add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPool {
    pub name: String,
    pub provider: String,
    pub slots: u32,
    pub reason: String,
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

/// Build a plan from a loaded config. Does not write.
#[must_use]
pub fn plan_migrate(config: &MailspaceConfig) -> MigratePlan {
    let Some(pump) = &config.pump else {
        return MigratePlan {
            notes: vec!["no [pump] table; nothing to migrate".into()],
            ..empty_plan()
        };
    };
    let mut plan = empty_plan();
    let dispatch = dispatch_pool(pump);
    assign_hand(config, pump, dispatch.as_deref(), &mut plan);
    assign_merge(config, pump, &mut plan);
    assign_gate(config, pump, dispatch.as_deref(), &mut plan);
    assign_slug_roles(config, &mut plan);
    fold_caps(config, pump, dispatch.as_deref(), &mut plan);
    if pump.planner.split_timeout_present {
        plan.renames.push(KeyRename {
            table: "pump.planner".into(),
            from: "split_timeout".into(),
            to: "split_hard_cap".into(),
            note: "split_timeout is deprecated; split_hard_cap wins when both are set".into(),
        });
    }
    if plan_is_empty(&plan) {
        plan.notes
            .push("already using role.pool assignments".into());
    }
    plan
}

fn empty_plan() -> MigratePlan {
    MigratePlan {
        assignments: Vec::new(),
        new_roles: Vec::new(),
        new_pools: Vec::new(),
        caps: Vec::new(),
        renames: Vec::new(),
        notes: Vec::new(),
    }
}

fn plan_is_empty(plan: &MigratePlan) -> bool {
    plan.assignments.is_empty()
        && plan.new_roles.is_empty()
        && plan.new_pools.is_empty()
        && plan.caps.is_empty()
        && plan.renames.is_empty()
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

fn assign_merge(config: &MailspaceConfig, pump: &PumpConfig, plan: &mut MigratePlan) {
    let Some(role) = find_role(config, "merge") else {
        return;
    };
    if role.pool.is_some() {
        return;
    }
    let name = host_pool_name(config);
    if !config.pools.contains_key(&name) && !plan.new_pools.iter().any(|pool| pool.name == name) {
        plan.new_pools.push(NewPool {
            name: name.clone(),
            provider: "local".into(),
            slots: pump.merge.lanes.max(1),
            reason: "[pump.merge] lanes (pump-host merge)".into(),
        });
    }
    plan.assignments.push(Assignment {
        role: role.name.clone(),
        pool: name,
        reason: "[pump.merge] lanes".into(),
    });
}

fn host_pool_name(config: &MailspaceConfig) -> String {
    for candidate in ["host", "pump-host", "local-host"] {
        if config.pools.contains_key(candidate) {
            let pool = &config.pools[candidate];
            if pool.provider == "local" {
                return candidate.to_string();
            }
        } else {
            return candidate.to_string();
        }
    }
    "host".into()
}

fn assign_gate(
    config: &MailspaceConfig,
    pump: &PumpConfig,
    dispatch: Option<&str>,
    plan: &mut MigratePlan,
) {
    let pool = pump.gate.pools.first().map(String::as_str).or(dispatch);
    let Some(pool) = pool else {
        return;
    };
    if let Some(role) = find_role(config, "gate") {
        if role.pool.is_none() {
            plan.assignments.push(Assignment {
                role: role.name.clone(),
                pool: pool.to_string(),
                reason: "[pump.gate] pools".into(),
            });
        }
        return;
    }
    plan.new_roles.push(NewRole {
        name: "gate".into(),
        kind: "gate".into(),
        pool: pool.to_string(),
        read_only: true,
    });
}

fn assign_slug_roles(config: &MailspaceConfig, plan: &mut MigratePlan) {
    for role in &config.identities {
        if role.pool.is_some() || already_assigned(plan, &role.name) {
            continue;
        }
        if role.status == "retired" {
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
    for pool in &plan.new_pools {
        lines.push(format!(
            "add [pools.{}] provider={} slots={}  ({})",
            pool.name, pool.provider, pool.slots, pool.reason
        ));
    }
    for role in &plan.new_roles {
        lines.push(format!(
            "add role {} kind={} pool={} read_only={}",
            role.name, role.kind, role.pool, role.read_only
        ));
    }
    for row in &plan.assignments {
        lines.push(format!(
            "set role {} pool={}  ({})",
            row.role, row.pool, row.reason
        ));
    }
    for cap in &plan.caps {
        lines.push(format!(
            "set [pools.{}.caps] {:?} = {}",
            cap.pool, cap.model, cap.cap
        ));
    }
    for rename in &plan.renames {
        lines.push(format!(
            "rename [{}] {} -> {}  ({})",
            rename.table, rename.from, rename.to, rename.note
        ));
    }
    lines.extend(plan.notes.iter().cloned());
    if lines.is_empty() {
        lines.push("no changes".into());
    }
    lines.join("\n")
}

/// Apply `plan` to `doc`.
pub fn apply_plan_to_document(
    doc: &mut DocumentMut,
    plan: &MigratePlan,
) -> Result<(), crate::error::VivariumError> {
    add_pools(doc, plan)?;
    add_roles(doc, plan)?;
    assign_roles(doc, plan)?;
    write_caps(doc, plan);
    rename_keys(doc, plan);
    Ok(())
}

fn rename_keys(doc: &mut DocumentMut, plan: &MigratePlan) {
    for rename in &plan.renames {
        let Some(table) = doc
            .get_mut("pump")
            .and_then(Item::as_table_mut)
            .and_then(|pump| pump.get_mut("planner"))
            .and_then(Item::as_table_mut)
        else {
            continue;
        };
        let Some(item) = table.remove(&rename.from) else {
            continue;
        };
        if table.get(&rename.to).is_none() {
            table.insert(&rename.to, item);
        }
    }
}

fn add_pools(doc: &mut DocumentMut, plan: &MigratePlan) -> Result<(), crate::error::VivariumError> {
    for pool in &plan.new_pools {
        crate::mailspace::config_edit::upsert_pool(
            doc,
            &crate::mailspace::config_edit::PoolAdd {
                name: pool.name.clone(),
                provider: pool.provider.clone(),
                slots: pool.slots,
                worktree_of: None,
                repo: None,
                image: None,
                root: None,
            },
        )?;
    }
    Ok(())
}

fn add_roles(doc: &mut DocumentMut, plan: &MigratePlan) -> Result<(), crate::error::VivariumError> {
    for role in &plan.new_roles {
        insert_identity(
            doc,
            &role.name,
            &IdentityPatch {
                kind: Some(Some(role.kind.clone())),
                pool: Some(Some(role.pool.clone())),
                read_only: Some(Some(role.read_only)),
                status: Some("active".into()),
                ..IdentityPatch::default()
            },
        )?;
    }
    Ok(())
}

fn assign_roles(
    doc: &mut DocumentMut,
    plan: &MigratePlan,
) -> Result<(), crate::error::VivariumError> {
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
