//! Config keys the current release ignores, and the local pool root that
//! keeps 15.0.0 slots where they are.

use toml_edit::{DocumentMut, Item, TableLike};

use crate::mailspace::MailspaceConfig;
use crate::pool::config::PoolConfig;
use crate::pool::slot::space_slug;

use super::migrate::{KeyRemoval, MigratePlan, RootSet};

/// The root 15.0.0 used for every local pool without one.
const V15_LOCAL_ROOT: &str = "~/vivi-pool";

const PLANNER_REASON: &str =
    "retired: planning runs as durable planner jobs on the pump-planner role's pool";
const GATE_REASON: &str = "ignored: the gate now runs after every merge batch that lands, \
     not on a merge count or a timer";

/// Keys that are always ignored: `(table, key, reason, behaviour_change)`.
const RETIRED: [(&str, &str, &str, bool); 5] = [
    ("pump.planner", "run", PLANNER_REASON, false),
    ("pump.planner", "charter", PLANNER_REASON, false),
    ("pump.gate", "every_merges", GATE_REASON, true),
    ("pump.gate", "every_seconds", GATE_REASON, true),
    (
        "pump",
        "priority_file",
        "retired: need priority lives in pump.db (`vivi pump priority`)",
        false,
    ),
];

/// Plan the removal of every ignored key the document still holds.
/// `merge_pool` is the pool the merge role has or is given; merge
/// concurrency follows its slots, so `[pump.merge] lanes` is ignored too.
pub(super) fn plan_removals(doc: &DocumentMut, merge_pool: &str, plan: &mut MigratePlan) {
    for (table, key, reason, behaviour_change) in RETIRED {
        push_removal(doc, table, key, reason, behaviour_change, plan);
    }
    let reason = format!(
        "ignored: merge concurrency follows the slots of the merge role's pool `{merge_pool}`"
    );
    push_removal(doc, "pump.merge", "lanes", &reason, false, plan);
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
        value: item.to_string().trim().to_string(),
        reason: reason.into(),
        behaviour_change,
    });
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
