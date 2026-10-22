//! Whether the pool behind a pump job role can actually run that role's jobs.
//!
//! The pump enqueues planner, merge, audit and gate jobs without a model; `vivi
//! pool dispatch` then resolves one from the role's band and the pool's bands
//! table. A role whose model cannot be resolved, or whose pool has no harness,
//! leaves its jobs pending for ever with nothing in the logs. This module asks
//! the dispatch resolver the same question, ahead of time.

use std::collections::BTreeMap;

use crate::pool::bands::parse_band;
use crate::pool::config::PoolConfig;
use crate::pool::role_dispatch::{self, RoleFacts};

use super::model::RoutedRole;
use super::validate::Issue;

/// Findings for one role the pump sends jobs to. A role with no pool, or
/// whose pool is not configured, is reported by the other checks.
pub(super) fn check_role_runs(
    role: Option<&RoutedRole>,
    pools: &BTreeMap<String, PoolConfig>,
    issues: &mut Vec<Issue>,
) {
    let Some(role) = role.filter(|role| role.status == "active") else {
        return;
    };
    let Some((pool_name, pool)) = role
        .pool
        .as_deref()
        .and_then(|name| pools.get(name).map(|pool| (name, pool)))
    else {
        return;
    };
    if pool.harness.is_none() {
        issues.push(Issue::not_ready(
            format!("pool {pool_name} harness"),
            format!(
                "role {} dispatches to pool {pool_name}, which has no [pools.{pool_name}.harness] \
                 run command, so its pump jobs can never run: add the harness (name, run, models)",
                role.name
            ),
        ));
        return;
    }
    if let Err(error) = resolve_like_dispatch(role, pool_name, pool) {
        let band = role
            .model
            .as_deref()
            .and_then(parse_band)
            .map_or_else(|| String::from("P2"), |band| format!("P{band}"));
        issues.push(Issue::error(format!(
            "role {} has no model on pool {pool_name}, so its pump jobs would stay pending: \
             {error}; fix: add [pools.{pool_name}.bands] entries (`vivi pool bands set \
             {pool_name} {band} <model> <reasoning>`) and give the role a band they serve \
             (`vivi role set {} --model {band}`)",
            role.name, role.name
        )));
    }
}

/// The model `pool dispatch` would choose for a pump job addressed to `role`:
/// no `--model` flag, no `band:` line, the role's band against the pool.
fn resolve_like_dispatch(
    role: &RoutedRole,
    pool_name: &str,
    pool: &PoolConfig,
) -> Result<(), crate::error::VivariumError> {
    let facts = RoleFacts {
        name: role.name.clone(),
        kind: role.kind.clone(),
        band: role.model.clone(),
        charter: String::new(),
        read_only: role.read_only,
    };
    role_dispatch::resolve(
        pool,
        pool_name,
        &role_dispatch::Input {
            model_flag: None,
            read_only_flag: false,
            role: Some(&facts),
            body: "",
        },
    )
    .map(|_| ())
}

#[cfg(test)]
#[path = "pump_models_test.rs"]
mod tests;
