//! Combined roles + pools + pump validation.

use std::collections::BTreeMap;

use crate::error::VivariumError;
use crate::mailspace::MailspaceConfig;
use crate::pool::bands::parse_band;
use crate::pool::config::PoolConfig;
use crate::pool::slot::PoolId;
use crate::pump::config::PumpConfig;

use super::defaults::is_band_model;
use super::model::{RoleRouting, RoutedRole, from_mailspace};
use super::pump_roles::pump_role_checks_of;

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Something is wrong.
    Error,
    /// Something is odd but works.
    Warning,
    /// Something the pump needs is absent. The project is not ready, not
    /// broken: supplying the item makes it ready.
    NotReady,
}

/// One validate finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub severity: Severity,
    pub message: String,
    /// The named missing item, set on a [`Severity::NotReady`] finding. Two
    /// findings with the same item are one thing missing.
    pub item: Option<String>,
}

impl Issue {
    pub(crate) fn error(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            item: None,
        }
    }

    pub(crate) fn not_ready(item: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::NotReady,
            message: message.into(),
            item: Some(item.into()),
        }
    }

    pub(crate) fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
            item: None,
        }
    }
}

/// Check roles, pools and pump together.
#[must_use]
pub fn validate(config: &MailspaceConfig) -> Vec<Issue> {
    let routing = from_mailspace(config);
    let mut issues = Vec::new();
    check_role_pools(&routing, &mut issues);
    check_pool_fallbacks(&routing, &mut issues);
    check_pool_tables(config, &mut issues);
    check_bands(&routing, &mut issues);
    check_models(config, &routing, &mut issues);
    check_read_only(&routing, &mut issues);
    if let Some(pump) = &config.pump {
        check_pump(pump, &routing, &config.pools, &mut issues);
    }
    issues
}

impl Severity {
    /// The lowercase tag used in reports: `error`, `warning`, `not ready`.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::NotReady => "not ready",
        }
    }
}

/// Format findings for humans, one `tag: message` line each.
#[must_use]
pub fn format_report(issues: &[Issue]) -> String {
    let mut lines = Vec::new();
    for issue in issues {
        lines.push(format!("{}: {}", issue.severity.tag(), issue.message));
    }
    lines.join("\n")
}

/// The not-ready findings, in report order.
pub fn not_ready(issues: &[Issue]) -> impl Iterator<Item = &Issue> {
    issues
        .iter()
        .filter(|issue| issue.severity == Severity::NotReady)
}

/// The one-line form of the not-ready findings, or `None` when none: `not
/// ready: <item>, <item>`.
#[must_use]
pub fn not_ready_summary(issues: &[Issue]) -> Option<String> {
    let items: Vec<&str> = not_ready(issues)
        .map(|issue| issue.item.as_deref().unwrap_or(&issue.message))
        .collect();
    (!items.is_empty()).then(|| format!("not ready: {}", items.join(", ")))
}

/// Fail when any finding is an error. Missing (not-ready) items are not
/// errors.
pub fn require_ok(issues: &[Issue]) -> Result<(), VivariumError> {
    let errors: Vec<&str> = issues
        .iter()
        .filter(|issue| issue.severity == Severity::Error)
        .map(|issue| issue.message.as_str())
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(VivariumError::Config(errors.join("; ")))
    }
}

fn check_role_pools(routing: &RoleRouting, issues: &mut Vec<Issue>) {
    for role in &routing.roles {
        let Some(pool) = role.pool.as_deref() else {
            continue;
        };
        if !routing.pools.contains_key(pool) {
            issues.push(Issue::error(format!(
                "role {} names unknown pool `{pool}`",
                role.name
            )));
        }
    }
}

/// Run each pool table's own checks, the ones every `vivi pool` command runs
/// when it starts. Without this a table such as one holding an empty model
/// slug passes validation and then silently stalls dispatch.
fn check_pool_tables(config: &MailspaceConfig, issues: &mut Vec<Issue>) {
    let mailspace_id = config.id.as_deref().unwrap_or_default();
    for (name, pool) in &config.pools {
        if let Err(error) = pool.validate(&PoolId::new(&config.name, mailspace_id, name)) {
            issues.push(Issue::error(match error {
                VivariumError::Config(message) => message,
                other => other.to_string(),
            }));
        }
    }
}

fn check_pool_fallbacks(routing: &RoleRouting, issues: &mut Vec<Issue>) {
    for (name, pool) in &routing.pools {
        if let Some(fallback) = pool.fallback.as_deref()
            && !routing.pools.contains_key(fallback)
        {
            issues.push(Issue::error(format!(
                "pool {name} falls back to unknown pool `{fallback}`"
            )));
        }
    }
    report_fallback_cycles(routing, issues);
}

fn report_fallback_cycles(routing: &RoleRouting, issues: &mut Vec<Issue>) {
    let mut reported = std::collections::BTreeSet::new();
    for start in routing.pools.keys() {
        let mut positions = std::collections::BTreeMap::new();
        let mut path = Vec::new();
        let mut next = Some(start.as_str());
        while let Some(name) = next {
            if let Some(&index) = positions.get(name) {
                let mut cycle = path[index..].to_vec();
                cycle.sort_unstable();
                if reported.insert(cycle.clone()) {
                    issues.push(Issue::error(format!(
                        "pool fallback cycle includes [{}]",
                        cycle.join(", ")
                    )));
                }
                break;
            }
            let Some(pool) = routing.pools.get(name) else {
                break;
            };
            positions.insert(name, path.len());
            path.push(name);
            next = pool.fallback.as_deref();
        }
    }
}

fn check_bands(routing: &RoleRouting, issues: &mut Vec<Issue>) {
    let any_bands = routing.pools.values().any(|pool| pool.has_bands);
    for role in &routing.roles {
        let Some(model) = role.model.as_deref() else {
            continue;
        };
        if parse_band(model).is_none() {
            continue;
        }
        push_band_issue(routing, role, model, any_bands, issues);
    }
}

fn push_band_issue(
    routing: &RoleRouting,
    role: &RoutedRole,
    model: &str,
    any_bands: bool,
    issues: &mut Vec<Issue>,
) {
    match role.pool.as_deref() {
        Some(pool)
            if routing
                .pools
                .get(pool)
                .is_some_and(|entry| !entry.has_bands) =>
        {
            let fallback = routing
                .pools
                .get(pool)
                .is_some_and(|entry| !entry.caps.is_empty());
            let message = format!(
                "role {} uses band `{model}` but [pools.{pool}] has no bands table; \
                 add one with `vivi pool bands set {pool} {} <model> <reasoning>`",
                role.name,
                band_key(model)
            );
            issues.push(if fallback {
                Issue::warning(message)
            } else {
                Issue::not_ready(format!("pool {pool} bands"), message)
            });
        }
        None if !any_bands => {
            issues.push(Issue::warning(format!(
                "role {} uses band `{model}` and no pool has a [pools.*.bands] table",
                role.name
            )));
        }
        _ => {}
    }
}

fn band_key(model: &str) -> String {
    parse_band(model).map_or_else(|| model.to_string(), |band| format!("P{band}"))
}

fn check_models(config: &MailspaceConfig, routing: &RoleRouting, issues: &mut Vec<Issue>) {
    for role in &routing.roles {
        let Some(pool_name) = role.pool.as_deref() else {
            continue;
        };
        let Some(pool) = config.pools.get(pool_name) else {
            continue;
        };
        for slug in slugs_of(role) {
            reject_unknown_slug(role, pool_name, pool, slug, issues);
        }
    }
}

fn slugs_of(role: &RoutedRole) -> Vec<&str> {
    let mut slugs: Vec<&str> = role
        .models
        .iter()
        .map(String::as_str)
        .filter(|model| !is_band_model(model))
        .collect();
    if let Some(model) = role.model.as_deref()
        && !is_band_model(model)
    {
        slugs.push(model);
    }
    slugs
}

fn reject_unknown_slug(
    role: &RoutedRole,
    pool_name: &str,
    pool: &PoolConfig,
    slug: &str,
    issues: &mut Vec<Issue>,
) {
    let Some(harness) = &pool.harness else {
        return;
    };
    if harness.models.iter().any(|offered| offered == slug) {
        return;
    }
    issues.push(Issue::error(format!(
        "role {} model `{slug}` is not in [pools.{pool_name}.harness].models",
        role.name
    )));
}

fn check_read_only(routing: &RoleRouting, issues: &mut Vec<Issue>) {
    for role in &routing.roles {
        if let Some(flag) = role.read_only
            && flag != role.default_read_only()
        {
            issues.push(Issue::warning(format!(
                "role {} has read_only = {flag} (kind {} defaults to {})",
                role.name,
                role.kind.as_deref().unwrap_or("none"),
                role.default_read_only()
            )));
        }
    }
}

fn check_pump(
    pump: &PumpConfig,
    routing: &RoleRouting,
    pools: &BTreeMap<String, PoolConfig>,
    issues: &mut Vec<Issue>,
) {
    need_role(routing, "hand", &pump.to, issues);
    issues.extend(
        pump_role_checks_of(pump, routing, pools)
            .into_iter()
            .flat_map(|check| check.issues),
    );
}

fn need_role(routing: &RoleRouting, label: &str, name: &str, issues: &mut Vec<Issue>) {
    match routing.lookup(name) {
        Some(role) if role.pool.is_some() => {}
        Some(role) => issues.push(Issue::not_ready(
            format!("{label} pool"),
            format!(
                "role {} has no pool; set one with `vivi role pool {} <pool>`",
                role.name, role.name
            ),
        )),
        None => issues.push(Issue::not_ready(
            format!("{label} pool"),
            format!("the pump needs a {label} role with a pool (add `{name}`)"),
        )),
    }
}
