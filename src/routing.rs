//! Role → pool routing: the mailspace map the pump and helpers share.
//!
//! A role may name a pool, an optional read-only flag, and a model, a band,
//! or a list of models. Pool capacity is `slots`; optional `[pools.X.caps]`
//! holds per-model caps inside that pool. When a role has no `pool`, the
//! pump falls back to the older `[pump.dispatch]` / `[pump.merge]` /
//! `[pump.gate]` tables and warns.
#![allow(clippy::missing_errors_doc)]

mod defaults;
mod diff;
mod migrate;
mod model;
mod show;
mod validate;

#[cfg(test)]
#[path = "routing/routing_test.rs"]
mod tests;

pub use defaults::{WRITE_KINDS, default_read_only, is_band_model, resolve_read_only};
pub use diff::unified_diff;
pub use migrate::{MigrateOutcome, MigratePlan, apply_plan_to_document, plan_migrate, render_plan};
pub use model::{
    RoleRouting, RoutedPool, RoutedRole, choose_model, from_mailspace, lanes_for, merge_lane_count,
};
pub use show::{ConfigOverview, FlowOverview, RoleRow, overview, render_text};
pub use validate::{Issue, Severity, format_report, require_ok, validate};
