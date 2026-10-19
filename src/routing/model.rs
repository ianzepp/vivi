//! In-memory view of roles, pools, and how the pump picks a pool and model.

use std::collections::BTreeMap;

use crate::mailspace::{LocalIdentity, MailspaceConfig};
use crate::pool::PoolConfig;
use crate::pump::config::{Lane, PumpConfig};

use super::defaults::{default_read_only, is_band_model, resolve_read_only};

/// The mailspace routing table used by the pump and `vivi config`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleRouting {
    pub roles: Vec<RoutedRole>,
    pub pools: BTreeMap<String, RoutedPool>,
}

/// One role's routing fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedRole {
    pub name: String,
    pub kind: Option<String>,
    pub status: String,
    pub pool: Option<String>,
    pub read_only: Option<bool>,
    pub model: Option<String>,
    pub models: Vec<String>,
}

/// The slice of a pool the router needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedPool {
    pub slots: u32,
    pub provider: String,
    pub caps: BTreeMap<String, u32>,
    pub models: Vec<String>,
    pub has_bands: bool,
}

impl RoleRouting {
    /// The role named `name`, or whose kind is `name` when no name matches.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&RoutedRole> {
        self.roles
            .iter()
            .find(|role| role.name.eq_ignore_ascii_case(name))
            .or_else(|| self.roles.iter().find(|role| kind_is(role, name)))
    }

    /// The hand / unit-dispatch role (`[pump] to`).
    #[must_use]
    pub fn hand<'a>(&'a self, pump_to: &str) -> Option<&'a RoutedRole> {
        self.lookup(pump_to)
    }

    /// The merge role: named `merge`, or kind `merge`.
    #[must_use]
    pub fn merge(&self) -> Option<&RoutedRole> {
        self.lookup("merge")
    }

    /// The gate role: named `gate`, or kind `gate`.
    #[must_use]
    pub fn gate(&self) -> Option<&RoutedRole> {
        self.lookup("gate")
    }

    /// Assigned `pool` when it has a free slot; else the first listed pool
    /// with a free slot. `None` means wait.
    #[must_use]
    pub fn first_free<'a>(
        assigned: Option<&'a str>,
        listed: &'a [String],
        free: &BTreeMap<String, u32>,
    ) -> Option<String> {
        if let Some(pool) = assigned {
            return (free.get(pool).copied().unwrap_or(0) >= 1).then(|| pool.to_string());
        }
        listed
            .iter()
            .find(|pool| free.get(*pool).is_some_and(|&n| n >= 1))
            .cloned()
    }

    /// Whether `role` has an assigned pool.
    #[must_use]
    pub fn pool_of<'a>(&'a self, role: &'a RoutedRole) -> Option<&'a str> {
        role.pool
            .as_deref()
            .filter(|name| self.pools.contains_key(*name))
    }
}

fn kind_is(role: &RoutedRole, kind: &str) -> bool {
    role.kind
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case(kind))
}

/// Build routing from a loaded mailspace config.
#[must_use]
pub fn from_mailspace(config: &MailspaceConfig) -> RoleRouting {
    RoleRouting {
        roles: config.identities.iter().map(role_from).collect(),
        pools: config
            .pools
            .iter()
            .map(|(name, pool)| (name.clone(), pool_from(pool)))
            .collect(),
    }
}

fn role_from(role: &LocalIdentity) -> RoutedRole {
    RoutedRole {
        name: role.name.clone(),
        kind: role.kind.clone(),
        status: role.status.clone(),
        pool: role.pool.clone(),
        read_only: role.read_only,
        model: role.model.clone(),
        models: role.models.clone(),
    }
}

fn pool_from(pool: &PoolConfig) -> RoutedPool {
    RoutedPool {
        slots: pool.slots,
        provider: pool.provider.clone(),
        caps: pool.caps.clone(),
        models: pool
            .harness
            .as_ref()
            .map(|harness| harness.models.clone())
            .unwrap_or_default(),
        has_bands: !pool.bands.is_empty(),
    }
}

/// Merge lane count: the merge role's pool `slots`, else `[pump.merge] lanes`.
#[must_use]
pub fn merge_lane_count(routing: &RoleRouting, pump: &PumpConfig) -> u32 {
    routing
        .merge()
        .and_then(|role| role.pool.as_deref())
        .and_then(|name| routing.pools.get(name))
        .map_or_else(|| pump.merge.lanes.max(1), |pool| pool.slots.max(1))
}

/// The lanes that cap models inside `pool`: `[pools.X.caps]` when set,
/// otherwise the legacy `[pump.dispatch.lanes]`.
#[must_use]
pub fn lanes_for<'a>(
    routing: &'a RoleRouting,
    pump: &'a PumpConfig,
    pool: Option<&str>,
) -> Vec<Lane> {
    if let Some(name) = pool
        && let Some(entry) = routing.pools.get(name)
        && !entry.caps.is_empty()
    {
        return entry
            .caps
            .iter()
            .map(|(model, cap)| Lane {
                model: model.clone(),
                cap: *cap,
            })
            .collect();
    }
    pump.dispatch.lanes.clone()
}

/// Pick a model for one dispatch. `None` means every cap is full (wait).
#[must_use]
pub fn choose_model(
    role: Option<&RoutedRole>,
    lanes: &[Lane],
    inflight: &BTreeMap<String, u32>,
    attempts: u32,
    last_model: Option<&str>,
    last_resort: &str,
    max_attempts: u32,
) -> Option<String> {
    if attempts >= max_attempts.saturating_sub(1) && !last_resort.is_empty() {
        return Some(last_resort.to_string());
    }
    if let Some(model) = role.and_then(preferred_slug) {
        return under_cap(lanes, inflight, &model).then_some(model);
    }
    pick_lane(lanes, inflight, attempts, last_model)
}

fn preferred_slug(role: &RoutedRole) -> Option<String> {
    role.models
        .iter()
        .find(|model| !is_band_model(model))
        .cloned()
        .or_else(|| {
            role.model
                .as_deref()
                .filter(|model| !is_band_model(model))
                .map(str::to_string)
        })
}

fn under_cap(lanes: &[Lane], inflight: &BTreeMap<String, u32>, model: &str) -> bool {
    lanes
        .iter()
        .find(|lane| lane.model == model)
        .is_none_or(|lane| inflight.get(model).copied().unwrap_or(0) < lane.cap)
}

fn pick_lane(
    lanes: &[Lane],
    inflight: &BTreeMap<String, u32>,
    attempts: u32,
    last_model: Option<&str>,
) -> Option<String> {
    let mut order: Vec<&Lane> = lanes.iter().collect();
    if attempts % 2 == 1 {
        order.reverse();
    }
    for skip_last in [true, false] {
        for lane in &order {
            let used = inflight.get(lane.model.as_str()).copied().unwrap_or(0);
            if used < lane.cap && (!skip_last || Some(lane.model.as_str()) != last_model) {
                return Some(lane.model.clone());
            }
        }
    }
    None
}

impl RoutedRole {
    /// Effective read-only: explicit flag, else the kind default.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        resolve_read_only(false, self.read_only, self.kind.as_deref())
    }

    /// Kind default without the role's override.
    #[must_use]
    pub fn default_read_only(&self) -> bool {
        default_read_only(self.kind.as_deref())
    }
}
