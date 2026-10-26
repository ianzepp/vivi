//! Typed thinking-step budgets (the `[pump.dispatch.step_budgets]` table).
//!
//! Every dispatch runs under `pump.dispatch.budget` unless this table gives
//! the step its own cap. The canonical keys are the seven thinking steps —
//! `planner`, `admitter`, `slicer`, `auditor`, `head-cto`, `researcher` and
//! `judge` — plus `envelope-fix` for the correction step. Every value is
//! whole seconds. Fallback rules: an absent key falls back to
//! `pump.dispatch.budget` for the seven thinking steps and to 300 seconds
//! for `envelope-fix` (independent of `budget` and of every other
//! override); roles with no key here (cto, merge, hand, repairer) always use
//! the shared `budget`, so existing merge and explicit repair-cap rules keep
//! working unchanged. A forthcoming change retries one timed-out thinking
//! step once at twice its resolved budget, capped by the pool bound; this
//! table only fixes the first attempt.

use crate::pump::envelope::roles::RoleKind;

use super::DispatchConfig;

/// Per-step budget overrides (the `[pump.dispatch.step_budgets]` table).
/// Only the canonical keys parse; every value is whole seconds.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StepBudgets {
    /// Seconds the planner step may take; absent = `dispatch.budget`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planner: Option<u64>,
    /// Seconds the admitter step may take; absent = `dispatch.budget`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admitter: Option<u64>,
    /// Seconds the slicer step may take; absent = `dispatch.budget`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slicer: Option<u64>,
    /// Seconds the auditor step may take; absent = `dispatch.budget`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auditor: Option<u64>,
    /// Seconds the head-cto step may take; absent = `dispatch.budget`.
    /// `head-cto` is the only accepted spelling.
    #[serde(rename = "head-cto", skip_serializing_if = "Option::is_none")]
    pub head_cto: Option<u64>,
    /// Seconds the researcher step may take; absent = `dispatch.budget`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub researcher: Option<u64>,
    /// Seconds the judge step may take; absent = `dispatch.budget`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judge: Option<u64>,
    /// Seconds the envelope-fix (correction) step may take; absent = 300.
    /// Correction is independent of `dispatch.budget` and of every other
    /// override.
    #[serde(rename = "envelope-fix", skip_serializing_if = "Option::is_none")]
    pub envelope_fix: Option<u64>,
}

impl StepBudgets {
    /// Whether no override is set; the table is then omitted when written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl DispatchConfig {
    /// Seconds the role's step may take: the step's own
    /// `[pump.dispatch.step_budgets]` override for a thinking step, else the
    /// shared `budget`. Roles with no thinking key (cto, merge, hand,
    /// repairer) always resolve to the shared `budget`.
    #[must_use]
    pub fn step_budget(&self, role: RoleKind) -> u64 {
        let own = match role {
            RoleKind::Planner => self.step_budgets.planner,
            RoleKind::Admitter => self.step_budgets.admitter,
            RoleKind::Slicer => self.step_budgets.slicer,
            RoleKind::Auditor => self.step_budgets.auditor,
            RoleKind::HeadCeo => self.step_budgets.head_cto,
            RoleKind::Researcher => self.step_budgets.researcher,
            RoleKind::Judge => self.step_budgets.judge,
            RoleKind::Cto | RoleKind::Merge | RoleKind::Hand | RoleKind::Repairer => None,
        };
        own.unwrap_or(self.budget)
    }

    /// Seconds the envelope-fix (correction) step may take: the
    /// `envelope-fix` override, else 300 — independently of `budget` and of
    /// every other override.
    #[must_use]
    pub fn envelope_fix_budget(&self) -> u64 {
        self.step_budgets.envelope_fix.unwrap_or(300)
    }
}

#[cfg(test)]
#[path = "budgets_test.rs"]
mod tests;
