//! In-memory view of roles, pools, and how the pump picks a pool and model.

use std::collections::BTreeMap;

use crate::mailspace::shipped::PUMP_PLANNER_ROLE;
use crate::mailspace::{LocalIdentity, MailspaceConfig};
use crate::pool::PoolConfig;

use super::defaults::{default_read_only, resolve_read_only};

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
    pub fallback: Option<String>,
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

    /// The pump's planner role: the role named `pump-planner`. A role of kind
    /// `planner` under another name (a Tugboat planner seat) is not it.
    #[must_use]
    pub fn pump_planner(&self) -> Option<&RoutedRole> {
        self.roles
            .iter()
            .find(|role| role.name.eq_ignore_ascii_case(PUMP_PLANNER_ROLE))
    }

    /// The auditor role: named `auditor`, or kind `auditor`.
    #[must_use]
    pub fn auditor(&self) -> Option<&RoutedRole> {
        self.lookup("auditor")
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

    /// Configured pool order starting at `start`, ending at the first pool
    /// without a fallback. Unknown pools and cycles stop the walk safely.
    #[must_use]
    pub fn fallback_chain(&self, start: &str) -> Vec<String> {
        let mut chain = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut next = Some(start);
        while let Some(name) = next {
            let Some(pool) = self.pools.get(name) else {
                break;
            };
            if !seen.insert(name) {
                break;
            }
            chain.push(name.to_string());
            next = pool.fallback.as_deref();
        }
        chain
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
        fallback: pool.fallback.clone(),
        caps: pool.caps.clone(),
        models: pool
            .harness
            .as_ref()
            .map(|harness| harness.models.clone())
            .unwrap_or_default(),
        has_bands: !pool.bands.is_empty(),
    }
}

/// Merge lane count: the merge role's pool `slots`, else one.
#[must_use]
pub fn merge_lane_count(routing: &RoleRouting) -> u32 {
    routing
        .merge()
        .and_then(|role| role.pool.as_deref())
        .and_then(|name| routing.pools.get(name))
        .map_or(1, |pool| pool.slots.max(1))
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
