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

/// The role the research flow sends a need to for analysis.
pub const RESEARCHER_ROLE: &str = "researcher";

/// The role that admits or rejects a plan before slicing.
pub const ADMITTER_ROLE: &str = "admitter";

/// The role that cuts an admitted plan into slices.
pub const SLICER_ROLE: &str = "slicer";
/// The role that fixes a red main in the repair flow.
pub const REPAIRER_ROLE: &str = "repairer";

/// The shipped charter of the [`RESEARCHER_ROLE`] role.
pub const RESEARCHER_CHARTER: &str =
    include_str!("../../skills/vivi/references/charters/researcher.md");

/// The shipped charter of the [`ADMITTER_ROLE`] role.
pub const ADMITTER_CHARTER: &str =
    include_str!("../../skills/vivi/references/charters/admitter.md");

/// The shipped charter of the [`SLICER_ROLE`] role.
pub const SLICER_CHARTER: &str = include_str!("../../skills/vivi/references/charters/slicer.md");

/// The shipped charter of the [`REPAIRER_ROLE`] role.
pub const REPAIRER_CHARTER: &str =
    include_str!("../../skills/vivi/references/charters/repairer.md");

/// The role the build flow sends its CTO step to.
pub const HEAD_CTO_ROLE: &str = "head-cto";

/// The shipped charter of the [`HEAD_CTO_ROLE`] role.
pub const HEAD_CTO_CHARTER: &str =
    include_str!("../../skills/tugboat/references/charters/head-cto.md");

/// The shipped charter text for a shipped role name, or `None` for any other.
#[must_use]
pub fn shipped_charter(role: &str) -> Option<&'static str> {
    match role {
        PUMP_PLANNER_ROLE => Some(PUMP_PLANNER_CHARTER),
        "merge" => Some(MERGE_CHARTER),
        "auditor" => Some(AUDITOR_CHARTER),
        RESEARCHER_ROLE => Some(RESEARCHER_CHARTER),
        ADMITTER_ROLE => Some(ADMITTER_CHARTER),
        SLICER_ROLE => Some(SLICER_CHARTER),
        REPAIRER_ROLE => Some(REPAIRER_CHARTER),
        HEAD_CTO_ROLE => Some(HEAD_CTO_CHARTER),
        _ => None,
    }
}

/// Whether a charter still asks for the 19.x reply format: not blank, and
/// either it carries a `PUMP-PLAN-BEGIN` marker or it has no `+++` front
/// matter line (every v20 charter ends in a TOML reply envelope). A blank
/// charter is not flagged here; the doctor reports empty charters.
#[must_use]
pub fn pre_v20_charter(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    text.contains("PUMP-PLAN-BEGIN") || !text.lines().any(|line| line.trim() == "+++")
}

#[cfg(test)]
#[path = "shipped_test.rs"]
mod tests;
