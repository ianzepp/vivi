//! The mind's action surface and the gate every action passes.
//!
//! The surface is deliberately narrow: each [`Action`] maps onto one
//! server-backed move — a read of project state, a letter, a pump control,
//! a tuning change or a filed need — that the mind carries out through the
//! mail server's tools, the way any client does. The mind runs no CLI
//! command and never edits `.vivi`, git, or config directly.
//!
//! [`permit`] is a pure function of the configured stage, whether the
//! operator paused the mind, and the budget ledger, so the gate is
//! unit-testable without a server. Two buckets share one window rule: the
//! bare need handles for need-scoped moves, and one reserved per-project
//! key for filing and role/pool tuning.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::config::{AuthorityStage, Budgets};

/// Everything the mind may ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Read pump status, health and open steps.
    ReadStatus,
    /// Read a project's pump log tail.
    ReadLog,
    /// Read one need's record and state.
    ReadNeed,
    /// Read a need's mail thread.
    ReadNeedThread,
    /// Read a task's mail thread and attempt evidence.
    ReadTaskThread,
    /// Read a project's work-graph flow.
    ReadFlow,
    /// Read the project's mailspace configuration.
    ReadConfig,
    /// Read a work pool's state.
    ReadPool,
    /// Write a letter to the operator.
    LetterToOperator,
    /// Escalate: a letter to the operator flagged as needing a decision.
    Escalate,
    /// Pause one project's pump through the server.
    PausePump,
    /// Resume a pump the mind itself paused.
    ResumePump,
    /// Requeue one step through the server's pump action.
    Requeue,
    /// Ask for a replan of one need.
    Replan,
    /// Answer an operator question on one step.
    Answer,
    /// Unlock a stuck need.
    Unlock,
    /// Post a reply letter on a need's thread.
    ReplyNeed,
    /// Set a need's priority.
    SetPriority,
    /// Withdraw a need from the backlog.
    WithdrawNeed,
    /// Retune a role's model assignment.
    SetRoleModel,
    /// Retune a pool's capacity.
    SetPoolCap,
    /// Retune a pool's model band.
    SetPoolBand,
    /// File a new need to the project through the server.
    FileNeed,
    /// File a fix need to the vivi project through the server.
    FileFixNeed,
    /// Dispatch an existing need to the pump.
    DispatchNeed,
    /// Take a release the operator cut. Stub.
    Upgrade,
    /// Cut a release. Stub.
    SelfRelease,
}

impl Action {
    /// The kebab-case kind, as a turn writes it in an envelope.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadStatus => "read-status",
            Self::ReadLog => "read-log",
            Self::ReadNeed => "read-need",
            Self::ReadNeedThread => "read-need-thread",
            Self::ReadTaskThread => "read-task-thread",
            Self::ReadFlow => "read-flow",
            Self::ReadConfig => "read-config",
            Self::ReadPool => "read-pool",
            Self::LetterToOperator => "letter-to-operator",
            Self::Escalate => "escalate",
            Self::PausePump => "pause-pump",
            Self::ResumePump => "resume-pump",
            Self::Requeue => "requeue",
            Self::Replan => "replan",
            Self::Answer => "answer",
            Self::Unlock => "unlock",
            Self::ReplyNeed => "reply-need",
            Self::SetPriority => "set-priority",
            Self::WithdrawNeed => "withdraw-need",
            Self::SetRoleModel => "set-role-model",
            Self::SetPoolCap => "set-pool-cap",
            Self::SetPoolBand => "set-pool-band",
            Self::FileNeed => "file-need",
            Self::FileFixNeed => "file-fix-need",
            Self::DispatchNeed => "dispatch-need",
            Self::Upgrade => "upgrade",
            Self::SelfRelease => "self-release",
        }
    }

    /// The lowest stage that opens this action.
    #[must_use]
    pub fn required_stage(self) -> AuthorityStage {
        match self {
            Self::ReadStatus
            | Self::ReadLog
            | Self::ReadNeed
            | Self::ReadNeedThread
            | Self::ReadTaskThread
            | Self::ReadFlow
            | Self::ReadConfig
            | Self::ReadPool
            | Self::LetterToOperator
            | Self::Escalate => AuthorityStage::Observe,
            Self::PausePump
            | Self::ResumePump
            | Self::Requeue
            | Self::Replan
            | Self::Answer
            | Self::Unlock
            | Self::ReplyNeed
            | Self::SetPriority
            | Self::WithdrawNeed
            | Self::SetRoleModel
            | Self::SetPoolCap
            | Self::SetPoolBand => AuthorityStage::Unstick,
            Self::FileNeed | Self::FileFixNeed | Self::DispatchNeed => AuthorityStage::FileFixes,
            Self::Upgrade => AuthorityStage::UpgradeOnApproval,
            Self::SelfRelease => AuthorityStage::SelfRelease,
        }
    }

    /// Whether the action spends the per-need budget.
    #[must_use]
    pub fn need_scoped(self) -> bool {
        matches!(
            self,
            Self::Requeue
                | Self::Replan
                | Self::Answer
                | Self::Unlock
                | Self::ReplyNeed
                | Self::SetPriority
                | Self::WithdrawNeed
                | Self::DispatchNeed
        )
    }

    /// Which budget the action draws down. Filing and the role/pool tuning
    /// changes count against the project, not one need.
    #[must_use]
    pub fn budget(self) -> BudgetClass {
        match self {
            Self::FileNeed
            | Self::FileFixNeed
            | Self::SetRoleModel
            | Self::SetPoolCap
            | Self::SetPoolBand => BudgetClass::PerProject,
            Self::Requeue
            | Self::Replan
            | Self::Answer
            | Self::Unlock
            | Self::ReplyNeed
            | Self::SetPriority
            | Self::WithdrawNeed
            | Self::DispatchNeed => BudgetClass::PerNeed,
            Self::ReadStatus
            | Self::ReadLog
            | Self::ReadNeed
            | Self::ReadNeedThread
            | Self::ReadTaskThread
            | Self::ReadFlow
            | Self::ReadConfig
            | Self::ReadPool
            | Self::LetterToOperator
            | Self::Escalate
            | Self::PausePump
            | Self::ResumePump
            | Self::Upgrade
            | Self::SelfRelease => BudgetClass::Unbudgeted,
        }
    }

    /// Whether the action changes server or project state. Reads and the
    /// operator letters do not.
    #[must_use]
    pub fn mutates(self) -> bool {
        !matches!(
            self,
            Self::ReadStatus
                | Self::ReadLog
                | Self::ReadNeed
                | Self::ReadNeedThread
                | Self::ReadTaskThread
                | Self::ReadFlow
                | Self::ReadConfig
                | Self::ReadPool
                | Self::LetterToOperator
                | Self::Escalate
        )
    }
}

/// Which budget an action draws down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetClass {
    /// No budget applies: reads, letters, pause and resume.
    Unbudgeted,
    /// Counts against the named need's per-need budget.
    PerNeed,
    /// Counts against the project's filing-and-tuning budget.
    PerProject,
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
    /// The project's shared filing-and-tuning budget for the window is
    /// spent. The `project` is the reserved [`project_key`] ledger key,
    /// never a bare need handle.
    ProjectBudget {
        project: String,
        used: u32,
        limit: u32,
    },
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
            Self::ProjectBudget {
                project,
                used,
                limit,
            } => write!(f, "{project} budget spent ({used}/{limit})"),
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
    window_check(ledger, need, gate.budgets, gate.now, |used, limit| {
        Denial::Budget {
            need: need.to_string(),
            used,
            limit,
        }
    })
}

/// The project half of the budget: filing and role/pool tuning draw down
/// one shared window per watched project, on the reserved [`project_key`]
/// ledger key. The stage and pause gates are [`permit`]'s; this is the
/// window check alone. Bare-need entries never count here.
///
/// # Errors
/// Returns [`Denial::ProjectBudget`] when the project's window is spent.
pub fn permit_project(gate: &Gate<'_>, ledger: &Ledger, project: &Path) -> Result<(), Denial> {
    let key = project_key(project);
    window_check(ledger, &key, gate.budgets, gate.now, |used, limit| {
        Denial::ProjectBudget {
            project: key.clone(),
            used,
            limit,
        }
    })
}

/// The ledger key of a project's shared filing-and-tuning window. The
/// `project:` prefix carries `:` and the path's separators, which cannot
/// pass the handle validator, so a project key can never collide with a
/// bare need handle and legacy bare-need entries keep their meaning.
#[must_use]
pub fn project_key(project: &Path) -> String {
    format!("project:{}", project.display())
}

/// The one window rule both buckets share: count `key`'s entries inside the
/// configured window and refuse with `denial(used, limit)` once they reach
/// the limit.
fn window_check(
    ledger: &Ledger,
    key: &str,
    budgets: &Budgets,
    now: u64,
    denial: impl FnOnce(u32, u32) -> Denial,
) -> Result<(), Denial> {
    let used = used_in_window(ledger, key, now, budgets.per_need_window_secs);
    if used >= budgets.per_need_actions {
        return Err(denial(used, budgets.per_need_actions));
    }
    Ok(())
}

/// Record a permitted action in its bucket — the bare need handle or the
/// reserved project key — and drop entries older than the window.
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
