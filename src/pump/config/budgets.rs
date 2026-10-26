//! Typed thinking-step budgets (the `[pump.dispatch.step_budgets]` table).
//!
//! Every dispatch runs under `pump.dispatch.budget` (default 600 s) unless
//! this table gives the step its own cap. The canonical table — every key
//! optional, every value whole seconds:
//!
//! ```toml
//! [pump.dispatch.step_budgets]
//! planner = 900       # absent: `pump.dispatch.budget`
//! admitter = 600      # absent: `pump.dispatch.budget`
//! slicer = 1200       # absent: `pump.dispatch.budget`
//! auditor = 900       # absent: `pump.dispatch.budget`
//! head-cto = 1800     # absent: `pump.dispatch.budget`; only spelling
//! researcher = 900    # absent: `pump.dispatch.budget`
//! judge = 600         # absent: `pump.dispatch.budget`
//! envelope-fix = 300  # absent: 300, independent of `budget`
//! ```
//!
//! Fallback rules: an absent thinking key resolves to the shared
//! `pump.dispatch.budget`; `envelope-fix` resolves to 300 seconds
//! independently of `budget` and of every other override. Roles with no key
//! here (cto, merge, hand, repairer) always use the shared `budget`, so
//! existing merge and explicit repair-cap rules keep working unchanged.
//! These budgets are wall-clock caps per dispatch step, unrelated to
//! `pump.slice.minutes`, which sizes the work units the slicer aims for;
//! neither setting feeds the other.
//!
//! A budget kill gets one retry at a raised budget before the step counts
//! as failed: `pump::flow::budget_retry` runs the retry at twice the
//! step's resolved budget, capped at the pool bound
//! (`pool::dispatch::BUDGET_RANGE`), and marks it spent so a second kill
//! is final. This table fixes the first attempt only; the raise is
//! computed per dispatch.
//!
//! Ruling: "The slicer's ~300 s stays a sizing target, never a hard stop.
//! Thinking steps (planner, admitter, slicer, auditor, head-cto) get their
//! own budgets. A budget kill gets one retry at a raised budget before the
//! step counts as failed."

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
