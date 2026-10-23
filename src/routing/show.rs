//! `vivi config show` projection.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::mailspace::MailspaceConfig;
use crate::pump::config::PumpConfig;

use super::model::{RoleRouting, RoutedRole, from_mailspace, merge_lane_count};

/// The resolved role → pool table plus the merge/gate flow.
#[derive(Debug, Clone, Serialize)]
pub struct ConfigOverview {
    pub roles: Vec<RoleRow>,
    pub flow: FlowOverview,
    pub warnings: Vec<String>,
}

/// One row of `role → kind → pool → slots → models → read_only`.
#[derive(Debug, Clone, Serialize)]
pub struct RoleRow {
    pub role: String,
    pub kind: String,
    pub pool: String,
    pub slots: String,
    pub free: Option<u32>,
    pub total: Option<u32>,
    pub models: String,
    pub read_only: bool,
    pub source: String,
}

/// Merge and gate flow as the pump will run it.
#[derive(Debug, Clone, Serialize)]
pub struct FlowOverview {
    pub merge_role: String,
    pub merge_pool: String,
    pub merge_lanes: u32,
    pub gate_role: String,
    pub gate_pool: String,
    pub main: String,
}

/// Build the overview. `free` is live slot counts when `--resolved`.
#[must_use]
pub fn overview(config: &MailspaceConfig, free: Option<&BTreeMap<String, u32>>) -> ConfigOverview {
    let routing = from_mailspace(config);
    let pump = config.pump.as_ref();
    let warnings = super::validate::validate(config)
        .into_iter()
        .map(|issue| issue.message)
        .collect();
    ConfigOverview {
        roles: routing
            .roles
            .iter()
            .filter(|role| role.status != "retired")
            .map(|role| row(&routing, role, pump, free))
            .collect(),
        flow: flow(&routing, pump),
        warnings,
    }
}

fn row(
    routing: &RoleRouting,
    role: &RoutedRole,
    pump: Option<&PumpConfig>,
    free: Option<&BTreeMap<String, u32>>,
) -> RoleRow {
    let (pool, source) = pool_and_source(routing, role, pump);
    let total = pool
        .as_deref()
        .and_then(|name| routing.pools.get(name))
        .map(|entry| entry.slots);
    let live = pool
        .as_deref()
        .and_then(|name| free.and_then(|map| map.get(name)).copied());
    RoleRow {
        role: role.name.clone(),
        kind: role.kind.clone().unwrap_or_else(|| "-".into()),
        pool: pool.clone().unwrap_or_else(|| "-".into()),
        slots: slots_text(live, total),
        free: live,
        total,
        models: models_text(role),
        read_only: role.is_read_only(),
        source,
    }
}

fn pool_and_source(
    routing: &RoleRouting,
    role: &RoutedRole,
    pump: Option<&PumpConfig>,
) -> (Option<String>, String) {
    if let Some(pool) = &role.pool {
        return (Some(pool.clone()), "role.pool".into());
    }
    if let Some(pump) = pump {
        return legacy_pool(routing, role, pump);
    }
    (None, "unset".into())
}

fn legacy_pool(
    _routing: &RoleRouting,
    role: &RoutedRole,
    pump: &PumpConfig,
) -> (Option<String>, String) {
    if role.name.eq_ignore_ascii_case(&pump.to)
        && let Some(kind) = pump.dispatch.kinds.get(&pump.dispatch.default_kind)
        && let Some(pool) = kind.pools.first()
    {
        return (Some(pool.clone()), "pump.dispatch.kinds".into());
    }
    if role.name.eq_ignore_ascii_case("merge") || kind_is(role, "merge") {
        return (Some("(host)".into()), "merge role".into());
    }
    (None, "legacy".into())
}

fn kind_is(role: &RoutedRole, kind: &str) -> bool {
    role.kind
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case(kind))
}

fn slots_text(free: Option<u32>, total: Option<u32>) -> String {
    match (free, total) {
        (Some(free), Some(total)) => format!("{free}/{total}"),
        (None, Some(total)) => format!("-/{total}"),
        _ => "-".into(),
    }
}

fn models_text(role: &RoutedRole) -> String {
    if !role.models.is_empty() {
        return role.models.join(",");
    }
    role.model.clone().unwrap_or_else(|| "-".into())
}

fn flow(routing: &RoleRouting, pump: Option<&PumpConfig>) -> FlowOverview {
    let Some(pump) = pump else {
        return FlowOverview {
            merge_role: "-".into(),
            merge_pool: "-".into(),
            merge_lanes: 0,
            gate_role: "-".into(),
            gate_pool: "-".into(),
            main: "-".into(),
        };
    };
    let merge = routing.merge();
    let gate = routing.gate();
    FlowOverview {
        merge_role: merge.map_or_else(|| "merge".into(), |role| role.name.clone()),
        merge_pool: merge
            .and_then(|role| role.pool.clone())
            .unwrap_or_else(|| "(host)".into()),
        merge_lanes: merge_lane_count(routing),
        gate_role: gate.map_or_else(|| "gate".into(), |role| role.name.clone()),
        gate_pool: gate
            .and_then(|role| role.pool.clone())
            .unwrap_or_else(|| "-".into()),
        main: pump.main.clone(),
    }
}

/// Text table for humans.
#[must_use]
pub fn render_text(overview: &ConfigOverview) -> String {
    let mut lines = vec![
        "role            kind      pool        slots   models                   ro   source".into(),
        "--------------  --------  ----------  ------  -----------------------  ---  ----------------".into(),
    ];
    for row in &overview.roles {
        lines.push(format!(
            "{:<14}  {:<8}  {:<10}  {:<6}  {:<23}  {:<3}  {}",
            clip(&row.role, 14),
            clip(&row.kind, 8),
            clip(&row.pool, 10),
            clip(&row.slots, 6),
            clip(&row.models, 23),
            if row.read_only { "yes" } else { "no" },
            row.source
        ));
    }
    lines.push(String::new());
    lines.push(flow_text(&overview.flow));
    for warning in &overview.warnings {
        lines.push(format!("warning: {warning}"));
    }
    lines.join("\n")
}

fn flow_text(flow: &FlowOverview) -> String {
    format!(
        "flow: {} @ {} ({} lanes) → shared checks → {} @ {} → {}",
        flow.merge_role,
        flow.merge_pool,
        flow.merge_lanes,
        flow.gate_role,
        flow.gate_pool,
        flow.main
    )
}

fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    text.chars()
        .take(width.saturating_sub(1))
        .collect::<String>()
        + "…"
}
