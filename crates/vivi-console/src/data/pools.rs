//! The pools-and-roles page's data, read from the project config.
//!
//! Only known, non-secret fields are copied out: a field is on the list
//! below or it is not shown. Nothing here reads a command, an environment
//! variable, an endpoint or a credential command, and a name that looks like
//! a secret is hidden whatever it holds ([`is_secret_name`]).

use std::path::Path;

use vivi_core::mailspace::MailspaceConfig;
use vivi_error::VivariumError;

use super::config;

/// Shown in place of a name that looks like a secret.
pub const HIDDEN: &str = "(hidden)";

/// Words that mark a field name as secret. Matched case-insensitively
/// anywhere in the name: `api_key`, `key_cmd`, `AUTH_TOKEN`, `password`.
const SECRET_WORDS: [&str; 5] = ["key", "token", "secret", "password", "passwd"];

/// Whether a field or setting name looks like it holds a secret.
#[must_use]
pub fn is_secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SECRET_WORDS.iter().any(|word| lower.contains(word))
}

/// `name`, or [`HIDDEN`] when it looks like a secret.
#[must_use]
pub fn shown(name: &str) -> String {
    if is_secret_name(name) {
        HIDDEN.to_string()
    } else {
        name.to_string()
    }
}

/// One role: a seat of the mailspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleView {
    pub name: String,
    pub kind: Option<String>,
    pub status: String,
    pub pool: Option<String>,
    /// The model and the preferred models, without repeats.
    pub models: Vec<String>,
    pub provider: Option<String>,
    pub harness: Option<String>,
    /// How long the role may stay silent (`5m`, `2h`).
    pub cadence: Option<String>,
    pub labels: Vec<String>,
}

/// A slot declared with `[[slot]]` and routed to a pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredSlot {
    pub name: String,
    pub host: String,
}

/// One pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolView {
    pub name: String,
    pub provider: String,
    /// The `slots` cap.
    pub slots: u32,
    pub fallback: Option<String>,
    /// Per-model concurrency caps.
    pub caps: Vec<(String, u32)>,
    /// Band and its choices, as `model reasoning`.
    pub bands: Vec<(String, Vec<String>)>,
    /// The harness name and the models it may be given.
    pub harness: Option<(String, Vec<String>)>,
    pub declared_slots: Vec<DeclaredSlot>,
    /// Roles whose `pool` names this one.
    pub roles: Vec<String>,
}

/// A unit kind of the pump's dispatch: which pools it goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitKind {
    pub name: String,
    pub pools: Vec<String>,
    /// Units of the kind allowed in flight at once.
    pub max: Option<u32>,
}

/// The pump's dispatch lanes and unit kinds, when a pump is configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchView {
    /// Seconds between pump ticks.
    pub tick: u64,
    /// Model and the units it may hold at once.
    pub lanes: Vec<(String, u32)>,
    pub default_kind: String,
    pub kinds: Vec<UnitKind>,
}

/// Everything the page shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolsData {
    pub roles: Vec<RoleView>,
    pub pools: Vec<PoolView>,
    pub dispatch: Option<DispatchView>,
}

/// Read the pools and roles of the project at `root`.
///
/// # Errors
/// Returns an error when the config cannot be read.
pub fn load(root: &Path) -> Result<PoolsData, VivariumError> {
    Ok(from_config(&config::load(root)?))
}

/// The page data of a parsed config.
#[must_use]
pub fn from_config(config: &MailspaceConfig) -> PoolsData {
    let mut roles: Vec<RoleView> = config.identities.iter().map(role_view).collect();
    roles.sort_by(|a, b| a.name.cmp(&b.name));
    let pools = config
        .pools
        .iter()
        .map(|(name, pool)| pool_view(config, name, pool, &roles))
        .collect();
    PoolsData {
        roles,
        pools,
        dispatch: config.pump.as_ref().map(dispatch_view),
    }
}

fn dispatch_view(pump: &vivi_core::pump::config::PumpConfig) -> DispatchView {
    DispatchView {
        tick: pump.tick,
        lanes: pump
            .dispatch
            .lanes
            .iter()
            .map(|lane| (lane.model.clone(), lane.cap))
            .collect(),
        default_kind: pump.dispatch.default_kind.clone(),
        kinds: pump
            .dispatch
            .kinds
            .iter()
            .map(|(name, kind)| UnitKind {
                name: name.clone(),
                pools: kind.pools.clone(),
                max: kind.max,
            })
            .collect(),
    }
}

fn role_view(identity: &vivi_core::mailspace::LocalIdentity) -> RoleView {
    let mut models: Vec<String> = Vec::new();
    for model in identity.model.iter().chain(&identity.models) {
        if !models.contains(model) {
            models.push(model.clone());
        }
    }
    RoleView {
        name: identity.name.clone(),
        kind: identity.kind.clone(),
        status: identity.status.clone(),
        pool: identity.pool.clone(),
        models,
        provider: identity.provider.clone(),
        harness: identity.harness.clone(),
        cadence: identity.cadence.clone(),
        labels: identity.labels.clone(),
    }
}

fn pool_view(
    config: &MailspaceConfig,
    name: &str,
    pool: &vivi_core::pool::config::PoolConfig,
    roles: &[RoleView],
) -> PoolView {
    PoolView {
        name: name.to_string(),
        provider: pool.provider.clone(),
        slots: pool.slots,
        fallback: pool.fallback.clone(),
        caps: pool
            .caps
            .iter()
            .map(|(model, cap)| (shown(model), *cap))
            .collect(),
        bands: pool
            .bands
            .entries
            .iter()
            .map(|(band, choices)| {
                let choices = choices
                    .iter()
                    .map(|choice| format!("{} {}", choice.model, choice.reasoning))
                    .collect();
                (shown(band), choices)
            })
            .collect(),
        harness: pool
            .harness
            .as_ref()
            .map(|harness| (shown(&harness.name), harness.models.clone())),
        declared_slots: config
            .slots
            .iter()
            .filter(|slot| slot.pools.is_empty() || slot.pools.iter().any(|routed| routed == name))
            .map(|slot| DeclaredSlot {
                name: slot.name.clone(),
                host: slot.host.clone(),
            })
            .collect(),
        roles: roles
            .iter()
            .filter(|role| role.pool.as_deref() == Some(name))
            .map(|role| role.name.clone())
            .collect(),
    }
}

#[cfg(test)]
#[path = "pools_test.rs"]
pub(crate) mod tests;
