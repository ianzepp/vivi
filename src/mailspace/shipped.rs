//! Roles and charters Vivi ships with.

/// The role the pump sends durable planner jobs to.
pub const PUMP_PLANNER_ROLE: &str = "pump-planner";

/// The shipped charter of the [`PUMP_PLANNER_ROLE`] role.
pub const PUMP_PLANNER_CHARTER: &str =
    include_str!("../../skills/vivi/references/charters/pump-planner.md");

/// The shipped charter of the `merge` role.
pub const MERGE_CHARTER: &str = include_str!("../../skills/tugboat/references/charters/merge.md");

/// The shipped charter of the `auditor` role.
pub const AUDITOR_CHARTER: &str =
    include_str!("../../skills/tugboat/references/charters/auditor.md");
