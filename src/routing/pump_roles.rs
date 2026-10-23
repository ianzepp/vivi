//! The roles a configured pump needs, checked once for `vivi config validate`
//! and `vivi doctor`.

use std::collections::BTreeMap;

use crate::mailspace::MailspaceConfig;
use crate::mailspace::shipped::{
    ADMITTER_ROLE, HEAD_CTO_ROLE, PUMP_PLANNER_ROLE, REPAIRER_ROLE, RESEARCHER_ROLE, SLICER_ROLE,
};
use crate::pool::config::PoolConfig;
use crate::pump::config::PumpConfig;

use super::model::{RoleRouting, RoutedRole, from_mailspace};
use super::pump_models::check_role_runs;
use super::validate::Issue;

/// The findings for one requirement of the pump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PumpRoleCheck {
    /// What the requirement is about: `pump-planner`, `researcher`,
    /// `admitter`, `slicer`, `repairer`, `merge`, `merge kind`, `auditor`, `head-cto`,
    /// `hand kind`, or
    /// a role's `<role> model` (the role's pool can run its jobs).
    pub name: &'static str,
    /// Empty when the requirement is met. An issue is an error, or a
    /// not-ready item for something the project has not set up yet.
    pub issues: Vec<Issue>,
}

/// Check the roles a configured pump needs. Empty when there is no `[pump]`.
#[must_use]
pub fn pump_role_checks(config: &MailspaceConfig) -> Vec<PumpRoleCheck> {
    config
        .pump
        .as_ref()
        .map(|pump| pump_role_checks_of(pump, &from_mailspace(config), &config.pools))
        .unwrap_or_default()
}

pub(super) fn pump_role_checks_of(
    pump: &PumpConfig,
    routing: &RoleRouting,
    pools: &BTreeMap<String, PoolConfig>,
) -> Vec<PumpRoleCheck> {
    vec![
        collect("pump-planner", |issues| check_planner(routing, issues)),
        collect("researcher", |issues| {
            check_flow_role(routing, RESEARCHER_ROLE, None, issues);
        }),
        collect("admitter", |issues| {
            check_flow_role(routing, ADMITTER_ROLE, None, issues);
        }),
        collect("slicer", |issues| {
            check_flow_role(routing, SLICER_ROLE, None, issues);
        }),
        collect("repairer", |issues| {
            // Band P0 is set by seeding and not checked here. Only a role of
            // kind repairer may publish `vivi/repair/<segment>`, and it must
            // be able to write.
            check_flow_role(routing, REPAIRER_ROLE, Some("repairer"), issues);
        }),
        collect("merge", |issues| check_merge(routing, issues)),
        collect("merge kind", |issues| check_merge_kind(routing, issues)),
        collect("auditor", |issues| check_auditor(routing, issues)),
        collect("head-cto", |issues| {
            check_flow_role(routing, HEAD_CTO_ROLE, None, issues);
        }),
        collect("hand kind", |issues| check_hand_kind(pump, routing, issues)),
        // The hand role is left out: the pump names each unit's model itself,
        // from the pool's caps, so a hand needs no band.
        collect("pump-planner model", |issues| {
            check_role_runs(routing.pump_planner(), pools, issues);
        }),
        collect("researcher model", |issues| {
            check_flow_role_runs(routing.lookup(RESEARCHER_ROLE), pools, issues);
        }),
        collect("admitter model", |issues| {
            check_flow_role_runs(routing.lookup(ADMITTER_ROLE), pools, issues);
        }),
        collect("slicer model", |issues| {
            check_flow_role_runs(routing.lookup(SLICER_ROLE), pools, issues);
        }),
        collect("repairer model", |issues| {
            check_flow_role_runs(routing.lookup(REPAIRER_ROLE), pools, issues);
        }),
        collect("merge model", |issues| {
            check_role_runs(routing.merge(), pools, issues);
        }),
        collect("auditor model", |issues| {
            check_role_runs(routing.auditor(), pools, issues);
        }),
        collect("head-cto model", |issues| {
            check_flow_role_runs(routing.lookup(HEAD_CTO_ROLE), pools, issues);
        }),
        collect("gate model", |issues| {
            check_role_runs(routing.gate(), pools, issues);
        }),
    ]
}

fn collect(name: &'static str, check: impl FnOnce(&mut Vec<Issue>)) -> PumpRoleCheck {
    let mut issues = Vec::new();
    check(&mut issues);
    PumpRoleCheck { name, issues }
}

fn check_planner(routing: &RoleRouting, issues: &mut Vec<Issue>) {
    let Some(role) = routing.pump_planner() else {
        issues.push(Issue::not_ready(
            "pump-planner role",
            format!(
                "the pump needs an active {PUMP_PLANNER_ROLE} role assigned to a pool for \
                 durable planning jobs: run `vivi config migrate`"
            ),
        ));
        return;
    };
    if role.status != "active" {
        issues.push(Issue::error(format!(
            "role {} must be active to receive durable planning jobs",
            role.name
        )));
    }
    if role.pool.is_none() {
        issues.push(Issue::not_ready(
            "pump-planner pool",
            format!(
                "role {} must name a pool for durable planning jobs: run `vivi config migrate` \
                 or `vivi role pool {} <pool>`",
                role.name, role.name
            ),
        ));
    }
}

/// The flow roles (researcher, admitter, slicer, repairer, head-cto) will run durable jobs like
/// the planner: each needs an active role assigned to a pool. Until the flow
/// engine dispatches role steps, a missing role or pool is only a warning;
/// the pump holds for them once it really uses them. A role that publishes
/// a branch names its required `kind`: any other kind, or a read-only role,
/// is an error because its jobs would be refused.
fn check_flow_role(
    routing: &RoleRouting,
    name: &str,
    required_kind: Option<&str>,
    issues: &mut Vec<Issue>,
) {
    let Some(role) = routing.lookup(name) else {
        issues.push(Issue::warning(format!(
            "the pump flows need an active {name} role assigned to a pool for durable jobs \
             (required once the flow engine dispatches role steps): run `vivi config migrate`"
        )));
        return;
    };
    if role.status != "active" {
        issues.push(Issue::error(format!(
            "role {} must be active to receive durable {name} jobs",
            role.name
        )));
    }
    if let Some(kind) = required_kind {
        check_publisher(role, kind, issues);
    }
    if role.pool.is_none() {
        issues.push(Issue::warning(format!(
            "role {} must name a pool for durable {name} jobs (required once the flow engine \
             dispatches role steps): run `vivi config migrate` or `vivi role pool {} <pool>`",
            role.name, role.name
        )));
    }
}

/// A publishing flow role must have the matching kind and must not be
/// read-only.
fn check_publisher(role: &RoutedRole, kind: &str, issues: &mut Vec<Issue>) {
    if !has_kind(role.kind.as_deref(), kind) {
        issues.push(Issue::error(format!(
            "role {} has kind {}; only a {kind}-kind role may publish its branches, so its \
             jobs would be refused: run `vivi config migrate` or set kind = \"{kind}\"",
            role.name,
            role.kind.as_deref().unwrap_or("none")
        )));
    }
    if role.read_only == Some(true) {
        issues.push(Issue::error(format!(
            "role {} is read-only and cannot publish its {kind} branches",
            role.name
        )));
    }
}

/// A flow role's `<role> model` findings, as warnings for the same reason.
fn check_flow_role_runs(
    role: Option<&RoutedRole>,
    pools: &BTreeMap<String, PoolConfig>,
    issues: &mut Vec<Issue>,
) {
    let mut found = Vec::new();
    check_role_runs(role, pools, &mut found);
    issues.extend(found.into_iter().map(|issue| {
        Issue::warning(format!(
            "{} (required once the flow engine dispatches role steps)",
            issue.message
        ))
    }));
}

fn check_merge(routing: &RoleRouting, issues: &mut Vec<Issue>) {
    let Some(role) = routing.merge() else {
        issues.push(Issue::not_ready(
            "merge role",
            "the pump needs an active merge role assigned to a pool for durable merge jobs",
        ));
        return;
    };
    if role.status != "active" {
        issues.push(Issue::error(format!(
            "merge role {} must be active to receive durable merge jobs",
            role.name
        )));
    }
    if role.pool.is_none() {
        issues.push(Issue::not_ready(
            "merge pool",
            format!(
                "merge role {} must name a pool for durable merge jobs",
                role.name
            ),
        ));
    }
}

fn check_merge_kind(routing: &RoleRouting, issues: &mut Vec<Issue>) {
    let Some(role) = routing.merge() else {
        return;
    };
    if !has_kind(role.kind.as_deref(), "merge") {
        issues.push(Issue::error(format!(
            "merge role {} has kind {}; only a merge-kind role may publish to the integration \
             branch, so its merge jobs would be refused: run `vivi config migrate` or set \
             kind = \"merge\"",
            role.name,
            role.kind.as_deref().unwrap_or("none")
        )));
    }
}

/// Need acceptance audits have no fallback: they need an auditor with a pool.
fn check_auditor(routing: &RoleRouting, issues: &mut Vec<Issue>) {
    let Some(role) = routing.auditor().filter(|role| role.status == "active") else {
        issues.push(Issue::not_ready(
            "auditor role",
            "the pump needs an active auditor role assigned to a pool for need acceptance \
             audits, or every need stops before landing: run `vivi config migrate`",
        ));
        return;
    };
    if role.pool.is_none() {
        issues.push(Issue::not_ready(
            "auditor pool",
            format!(
                "auditor role {} must name a pool for need acceptance audits, or every need \
                 stops before landing: run `vivi config migrate` or `vivi role pool {} <pool>`",
                role.name, role.name
            ),
        ));
    }
}

/// Only a hand-kind role may publish a unit branch.
fn check_hand_kind(pump: &PumpConfig, routing: &RoleRouting, issues: &mut Vec<Issue>) {
    let Some(role) = routing.hand(&pump.to) else {
        return;
    };
    if !has_kind(role.kind.as_deref(), "hand") {
        issues.push(Issue::error(format!(
            "pump role {} has kind {}; only a hand-kind role may publish unit branches, so \
             every unit would fail with \"not authorized to publish\": run `vivi config \
             migrate` or set kind = \"hand\"",
            role.name,
            role.kind.as_deref().unwrap_or("none")
        )));
    }
}

fn has_kind(kind: Option<&str>, expected: &str) -> bool {
    kind.is_some_and(|kind| kind.trim().eq_ignore_ascii_case(expected))
}

#[cfg(test)]
#[path = "pump_roles_test.rs"]
mod tests;
