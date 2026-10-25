//! The mind's action surface and the gate every action passes.
//!
//! The surface is deliberately narrow: each [`Action`] maps onto a letter or
//! a pump control the pump already accepts (`vivi pump requeue|replan|
//! answer|unlock|pause`, a need filed through `vivi pump send`, a letter to
//! the operator). The mind never edits `.vivi`, git, or config directly.
//!
//! [`permit`] is a pure function of the configured stage, whether the
//! operator paused the mind, and the per-need budget ledger, so the gate is
//! unit-testable without a pump.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::config::{AuthorityStage, Budgets};

/// Everything the mind may ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Read pump status, health and logs.
    ReadStatus,
    /// Write a letter to the operator.
    LetterToOperator,
    /// Escalate: a letter to the operator flagged as needing a decision.
    Escalate,
    /// `vivi pump pause` on one project.
    PausePump,
    /// `vivi pump resume` on a pump the mind itself paused.
    ResumePump,
    /// `vivi pump requeue <step>`.
    Requeue,
    /// `vivi pump replan <need>`.
    Replan,
    /// `vivi pump answer <step> <word>`.
    Answer,
    /// `vivi pump unlock <need>`.
    Unlock,
    /// File a fix need to the vivi project through the normal pump.
    FileFixNeed,
    /// `vivi update --version <v>` after an operator-cut release. Stub.
    Upgrade,
    /// Cut a release. Stub.
    SelfRelease,
}

impl Action {
    /// The lowest stage that opens this action.
    #[must_use]
    pub fn required_stage(self) -> AuthorityStage {
        match self {
            Self::ReadStatus | Self::LetterToOperator | Self::Escalate => AuthorityStage::Observe,
            Self::PausePump
            | Self::ResumePump
            | Self::Requeue
            | Self::Replan
            | Self::Answer
            | Self::Unlock => AuthorityStage::Unstick,
            Self::FileFixNeed => AuthorityStage::FileFixes,
            Self::Upgrade => AuthorityStage::UpgradeOnApproval,
            Self::SelfRelease => AuthorityStage::SelfRelease,
        }
    }

    /// Whether the action spends the per-need budget.
    #[must_use]
    pub fn need_scoped(self) -> bool {
        matches!(
            self,
            Self::Requeue | Self::Replan | Self::Answer | Self::Unlock
        )
    }

    /// Whether the action changes pump or project state. Reads and letters
    /// do not.
    #[must_use]
    pub fn mutates(self) -> bool {
        !matches!(
            self,
            Self::ReadStatus | Self::LetterToOperator | Self::Escalate
        )
    }
}

/// Why an action was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Denial {
    /// The configured stage is below the action's stage.
    Stage {
        required: AuthorityStage,
        current: AuthorityStage,
    },
    /// The operator paused the mind: only reads and letters pass.
    Paused,
    /// A need-scoped action named no need.
    MissingNeed,
    /// The need's budget for the window is spent.
    Budget { need: String, used: u32, limit: u32 },
    /// The stage is open but this build does not carry the action out.
    NotImplemented,
}

impl std::fmt::Display for Denial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stage { required, current } => write!(
                f,
                "needs authority {} (configured {})",
                required.as_str(),
                current.as_str()
            ),
            Self::Paused => write!(f, "the mind is paused"),
            Self::MissingNeed => write!(f, "action names no need"),
            Self::Budget { need, used, limit } => {
                write!(f, "need {need} budget spent ({used}/{limit})")
            }
            Self::NotImplemented => write!(f, "not implemented in this build"),
        }
    }
}

/// Per-need action timestamps (epoch seconds), the budget ledger.
pub type Ledger = BTreeMap<String, Vec<u64>>;

/// What the gate needs to know about the mind right now.
#[derive(Debug, Clone, Copy)]
pub struct Gate<'a> {
    pub stage: AuthorityStage,
    pub paused: bool,
    pub budgets: &'a Budgets,
    pub now: u64,
}

/// Decide whether `action` on `need` may run, given what `ledger` has
/// already spent.
///
/// # Errors
/// Returns the [`Denial`] when the action is refused.
pub fn permit(
    gate: &Gate<'_>,
    ledger: &Ledger,
    action: Action,
    need: Option<&str>,
) -> Result<(), Denial> {
    let required = action.required_stage();
    if gate.stage < required {
        return Err(Denial::Stage {
            required,
            current: gate.stage,
        });
    }
    if !required.implemented() {
        return Err(Denial::NotImplemented);
    }
    if gate.paused && action.mutates() {
        return Err(Denial::Paused);
    }
    if !action.need_scoped() {
        return Ok(());
    }
    let Some(need) = need.filter(|need| !need.trim().is_empty()) else {
        return Err(Denial::MissingNeed);
    };
    let used = used_in_window(ledger, need, gate.now, gate.budgets.per_need_window_secs);
    if used >= gate.budgets.per_need_actions {
        return Err(Denial::Budget {
            need: need.to_string(),
            used,
            limit: gate.budgets.per_need_actions,
        });
    }
    Ok(())
}

/// Record a permitted need-scoped action in the ledger and drop entries
/// older than the window.
pub fn spend(ledger: &mut Ledger, need: &str, now: u64, window_secs: u64) {
    let entry = ledger.entry(need.to_string()).or_default();
    entry.retain(|at| now.saturating_sub(*at) < window_secs);
    entry.push(now);
}

fn used_in_window(ledger: &Ledger, need: &str, now: u64, window_secs: u64) -> u32 {
    let count = ledger.get(need).map_or(0, |times| {
        times
            .iter()
            .filter(|at| now.saturating_sub(**at) < window_secs)
            .count()
    });
    u32::try_from(count).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[path = "authority_test.rs"]
mod tests;
