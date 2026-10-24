//! Config keys the current release ignores, and the local pool root that
//! keeps 15.0.0 slots where they are.

use toml_edit::{DocumentMut, Item, TableLike};

use crate::mailspace::MailspaceConfig;
use crate::pool::config::PoolConfig;
use crate::pool::slot::space_slug;

use super::migrate::{KeyRemoval, MigratePlan, RootSet};

/// The root 15.0.0 used for every local pool without one.
const V15_LOCAL_ROOT: &str = "~/vivi-pool";

const PLANNER_REASON: &str = "not read by vivi 21: planning runs as durable planner jobs on the \
     pump-planner role's pool; the whole table is ignored";
const PROOF_REASON: &str = "not read by vivi 21: proof commands are the pump rungs \
     (`[pump.rungs]`); the whole table is ignored";
const PLAN_REASON: &str =
    "not read by vivi 21: plans are not checked against proof rules; the whole table is ignored";
const ROUTING_REASON: &str = "not read by vivi 21: jobs go to the hand role's pool, and \
     `vivi config migrate` carries lane caps and kind pools over to that pool";
const ATTEMPT_REASON: &str =
    "not read by vivi 21: there is no attempt, retry or overdue policy; remove it";
const REPAIR_REASON: &str = "not read by vivi 21: there are no quarantine repair units; remove it";
const ALARM_REASON: &str =
    "not read by vivi 21: there are no stall alarms; the whole table is ignored";
const PLANNER_CAP_REASON: &str =
    "not read by vivi 21: planner jobs take no cap or timeout from here; remove it";
const UNIT_FILE_REASON: &str =
    "not read by vivi 21: the pump no longer reserves paths or names fix units; remove it";
const INTAKE_REASON: &str = "retired: the pump plans exactly the needs with an open pump record \
     (`vivi pump send --need <handle>`), so no mark or date cutoff applies";
const GATE_REASON: &str = "retired: the pump no longer runs a gate; the whole table is ignored";
const BRANCH_REASON: &str = "retired: needs merge to main; there is no integration branch";
const PROMOTE_REASON: &str = "retired: there is no integration branch to promote from";
const MERGE_CHECKS_REASON: &str = "retired: the merge flow no longer reads a check list here";
const MERGE_BATCH_REASON: &str = "retired: the merge flow no longer batches or fans out needs";
const MERGE_TIMER_REASON: &str = "retired: the merge flow has its own step timeouts";
const MERGE_LANES_REASON: &str =
    "ignored: merge concurrency follows the slots of the merge role's pool";
const WORKSPACE_REASON: &str = "not read by vivi 21: merge lanes run on the merge role's pool, \
     not a lane RAM disk; the whole table is ignored";
const NEED_REASON: &str = "retired: no repair-round counter; the whole table is ignored";

/// Keys that are always ignored: `(table, key, reason, behaviour_change)`.
/// A key that holds a table retires the whole table. A `*` in the table path
/// stands for each `[pump.targets.<id>]`.
const RETIRED: [(&str, &str, &str, bool); 43] = [
    ("pump", "planner", PLANNER_REASON, false),
    ("pump", "proof", PROOF_REASON, false),
    ("pump", "plan", PLAN_REASON, false),
    ("pump", "alarm", ALARM_REASON, false),
    ("pump", "workspace", WORKSPACE_REASON, false),
    ("pump", "reserved_file", UNIT_FILE_REASON, false),
    ("pump", "fix_prefix", UNIT_FILE_REASON, false),
    ("pump.board", "planners_max", PLANNER_CAP_REASON, false),
    ("pump.board", "plan_timeout", PLANNER_CAP_REASON, false),
    ("pump.dispatch", "kinds", ROUTING_REASON, false),
    ("pump.dispatch", "default_kind", ROUTING_REASON, false),
    ("pump.dispatch", "lanes", ROUTING_REASON, false),
    ("pump.dispatch", "max_attempts", ATTEMPT_REASON, false),
    ("pump.dispatch", "per_tick", ATTEMPT_REASON, false),
    ("pump.dispatch", "transient_max", ATTEMPT_REASON, false),
    ("pump.dispatch", "transient_backoff", ATTEMPT_REASON, false),
    ("pump.dispatch", "overdue_grace", ATTEMPT_REASON, false),
    ("pump.dispatch", "last_resort", ATTEMPT_REASON, false),
    ("pump.merge", "repair_budget", REPAIR_REASON, false),
    (
        "pump.targets.*.merge",
        "repair_budget",
        REPAIR_REASON,
        false,
    ),
    ("pump.board", "mark", INTAKE_REASON, true),
    ("pump.board", "since", INTAKE_REASON, true),
    ("pump", "gate", GATE_REASON, true),
    ("pump", "branch", BRANCH_REASON, true),
    ("pump", "promote", PROMOTE_REASON, true),
    ("pump", "need", NEED_REASON, false),
    ("pump.merge", "batch", MERGE_BATCH_REASON, true),
    ("pump.merge", "need_parallel", MERGE_BATCH_REASON, true),
    ("pump.merge", "shared_checks", MERGE_CHECKS_REASON, true),
    ("pump.merge", "checks_for", MERGE_CHECKS_REASON, true),
    ("pump.merge", "proof_timeout", MERGE_TIMER_REASON, false),
    ("pump.merge", "remote_wait", MERGE_TIMER_REASON, false),
    (
        "pump",
        "priority_file",
        "retired: need priority lives in pump.db (`vivi pump priority`)",
        false,
    ),
    // Retired keys of each `[pump.targets.<id>]`: the table is `pump.targets.*`.
    ("pump.targets.*", "branch", BRANCH_REASON, true),
    ("pump.targets.*", "promote", PROMOTE_REASON, true),
    ("pump.targets.*", "gate", GATE_REASON, true),
    ("pump.targets.*.merge", "batch", MERGE_BATCH_REASON, true),
    (
        "pump.targets.*.merge",
        "need_parallel",
        MERGE_BATCH_REASON,
        true,
    ),
    (
        "pump.targets.*.merge",
        "shared_checks",
        MERGE_CHECKS_REASON,
        true,
    ),
    (
        "pump.targets.*.merge",
        "checks_for",
        MERGE_CHECKS_REASON,
        true,
    ),
    (
        "pump.targets.*.merge",
        "proof_timeout",
        MERGE_TIMER_REASON,
        false,
    ),
    (
        "pump.targets.*.merge",
        "remote_wait",
        MERGE_TIMER_REASON,
        false,
    ),
    ("pump.targets.*.merge", "lanes", MERGE_LANES_REASON, false),
];

/// Plan the removal of every ignored key the document still holds.
/// `merge_pool` is the pool the merge role has or is given; merge
/// concurrency follows its slots, so `[pump.merge] lanes` is ignored too.
pub(super) fn plan_removals(
    doc: &DocumentMut,
    config: &MailspaceConfig,
    merge_pool: &str,
    plan: &mut MigratePlan,
) {
    let target_ids: Vec<String> = table_at(doc, "pump.targets")
        .map(|targets| {
            targets
                .iter()
                .filter(|(_, item)| item.as_table_like().is_some())
                .map(|(id, _)| id.to_string())
                .collect()
        })
        .unwrap_or_default();
    for (table, key, reason, behaviour_change) in RETIRED {
        if table.contains('*') {
            for id in &target_ids {
                let path = table.replace('*', id);
                push_removal(doc, &path, key, reason, behaviour_change, plan);
            }
        } else {
            push_removal(doc, table, key, reason, behaviour_change, plan);
        }
    }
    plan_lanes_removal(doc, config, merge_pool, plan);
}

/// The old `[pump.merge] lanes` value as an integer, when the file sets one.
pub(super) fn raw_merge_lanes(doc: &DocumentMut) -> Option<u32> {
    let lanes = table_at(doc, "pump.merge")?.get("lanes")?.as_integer()?;
    u32::try_from(lanes).ok()
}

/// Merge concurrency follows the slots of the merge role's pool. A new pool
/// takes `lanes` as its slots; an existing pool keeps its own, and a pool
/// with fewer slots than `lanes` is a behaviour change, named with both values.
fn plan_lanes_removal(
    doc: &DocumentMut,
    config: &MailspaceConfig,
    merge_pool: &str,
    plan: &mut MigratePlan,
) {
    let slots = plan
        .new_pools
        .iter()
        .find(|pool| pool.name == merge_pool)
        .map(|pool| pool.slots)
        .or_else(|| config.pools.get(merge_pool).map(|pool| pool.slots));
    let lanes = raw_merge_lanes(doc);
    let short = match (lanes, slots) {
        (Some(lanes), Some(slots)) if lanes > slots => Some((lanes, slots)),
        _ => None,
    };
    let reason = match short {
        Some((lanes, slots)) => format!(
            "ignored: merge concurrency follows the slots of the merge role's pool \
             `{merge_pool}`; lanes was {lanes} but the pool has {slots} slot(s), so raise its \
             `slots` to keep {lanes} merges at once"
        ),
        None => format!(
            "ignored: merge concurrency follows the slots of the merge role's pool `{merge_pool}`"
        ),
    };
    push_removal(doc, "pump.merge", "lanes", &reason, short.is_some(), plan);
}

fn push_removal(
    doc: &DocumentMut,
    table: &str,
    key: &str,
    reason: &str,
    behaviour_change: bool,
    plan: &mut MigratePlan,
) {
    let Some(item) = table_at(doc, table).and_then(|found| found.get(key)) else {
        return;
    };
    plan.removals.push(KeyRemoval {
        table: table.into(),
        key: key.into(),
        value: describe(item),
        reason: reason.into(),
        behaviour_change,
    });
}

/// A key's value as written, or the key names of a whole table.
fn describe(item: &Item) -> String {
    if let Some(tables) = item.as_array_of_tables() {
        return format!("(array of {} table(s))", tables.len());
    }
    match item.as_table_like() {
        Some(table) => {
            let keys: Vec<&str> = table.iter().map(|(key, _)| key).collect();
            format!("(table: {})", keys.join(", "))
        }
        None => item.to_string().trim().to_string(),
    }
}

fn table_at<'a>(doc: &'a DocumentMut, path: &str) -> Option<&'a dyn TableLike> {
    let mut current: &dyn TableLike = doc.as_table();
    for part in path.split('.') {
        current = current.get(part)?.as_table_like()?;
    }
    Some(current)
}

fn table_at_mut<'a>(doc: &'a mut DocumentMut, path: &str) -> Option<&'a mut dyn TableLike> {
    let mut current: &mut dyn TableLike = doc.as_table_mut();
    for part in path.split('.') {
        current = current.get_mut(part)?.as_table_like_mut()?;
    }
    Some(current)
}

/// Delete the keys the plan removes.
pub(super) fn apply_removals(doc: &mut DocumentMut, plan: &MigratePlan) {
    for removal in &plan.removals {
        if let Some(table) = table_at_mut(doc, &removal.table) {
            table.remove(&removal.key);
        }
    }
}

/// Plan an explicit `root` for each local pool that has none, when 16 would
/// otherwise move its slots. 15.0.0 kept local slots in
/// `~/vivi-pool/<name slug>/<pool>/slot-N`; 16 defaults to
/// `<project>/.vivi/pool/<pool>/slot-N` and keys a shared root's directory on
/// the mailspace id. Only a mailspace without an id has the same directory
/// under `~/vivi-pool` in both, so only it can keep its slots. A pool the plan
/// adds gets the same root, so every local pool of the file sits together.
pub(super) fn plan_pool_roots(config: &MailspaceConfig, plan: &mut MigratePlan) {
    if config.id.is_some() {
        return;
    }
    let added = plan
        .new_pools
        .iter()
        .filter(|pool| pool.provider.eq_ignore_ascii_case("local"))
        .map(|pool| pool.name.as_str());
    let names: Vec<String> = unrooted_local_pools(config)
        .into_iter()
        .chain(added)
        .map(str::to_string)
        .collect();
    for name in names {
        plan.roots.push(RootSet {
            pool: name,
            root: V15_LOCAL_ROOT.into(),
            reason: "15.0.0 kept local slots in ~/vivi-pool; 16 defaults to <project>/.vivi/pool"
                .into(),
        });
    }
}

/// Name the local pools whose 15.0.0 slots 16 will not reuse. Called only
/// when the plan already changes something, so a migrated file stays quiet.
pub(super) fn review_slot_dirs(config: &MailspaceConfig, plan: &mut MigratePlan) {
    if config.id.is_none() {
        return;
    }
    let names = unrooted_local_pools(config);
    if names.is_empty() {
        return;
    }
    plan.review.push(format!(
        "local pool(s) {}: 15.0.0 slots in ~/vivi-pool/{}/<pool>/ are not reused (16 keys slot \
         directories on the mailspace id, default root <project>/.vivi/pool); new slots are \
         seeded on first dispatch. Remove the old directories when the pools are idle (worktree \
         pools: run `git worktree prune` in the source repository afterwards)",
        names.join(", "),
        space_slug(&config.name)
    ));
}

fn unrooted_local_pools(config: &MailspaceConfig) -> Vec<&str> {
    config
        .pools
        .iter()
        .filter(|(_, pool)| is_local(pool) && pool.local.root.is_none())
        .map(|(name, _)| name.as_str())
        .collect()
}

fn is_local(pool: &PoolConfig) -> bool {
    pool.provider.eq_ignore_ascii_case("local")
}

/// Write the explicit local pool roots.
pub(super) fn apply_roots(doc: &mut DocumentMut, plan: &MigratePlan) {
    for root in &plan.roots {
        let Some(pool) = doc
            .get_mut("pools")
            .and_then(Item::as_table_like_mut)
            .and_then(|pools| pools.get_mut(&root.pool))
            .and_then(Item::as_table_like_mut)
        else {
            continue;
        };
        if pool.get("local").is_none() {
            pool.insert("local", toml_edit::table());
        }
        if let Some(local) = pool.get_mut("local").and_then(Item::as_table_like_mut) {
            local.insert("root", toml_edit::value(root.root.as_str()));
        }
    }
}

#[cfg(test)]
#[path = "migrate_keys_test.rs"]
mod tests;
