//! Target-local intake: the literal Git heads a target accepts work from and
//! the rung that proves one before work starts on it.
//!
//! An intake value is a branch name, never a remote selector: `peer/main` is
//! the branch `main` on the peer's repository, not `origin/main`. Validation
//! is pure: it reads neither the repository nor the network.

use std::collections::BTreeSet;

use crate::pool::job::plain_ref;
use crate::pump::integration_branch::PUMP_BRANCH_PREFIX;

/// The rung that proves an intake branch before work starts on it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IntakeRung {
    /// Prove the intake branch with the `task` rung.
    Task,
    /// Prove the intake branch with the `need` rung.
    #[default]
    Need,
    /// Prove the intake branch with the `main` rung.
    Main,
}

impl IntakeRung {
    /// Every accepted spelling, in climbing order.
    pub const NAMES: [&'static str; 3] = ["task", "need", "main"];

    /// The rung's name, as `[pump.rungs]` and a target's rungs spell it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Need => "need",
            Self::Main => "main",
        }
    }
}

impl std::fmt::Display for IntakeRung {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Why one configured intake value is not a literal head a target may accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntakeIssue {
    /// An empty value.
    Empty,
    /// A value that is not a plain branch name.
    NotPlainRef,
    /// A Git URL, not a branch name.
    Url,
    /// A refspec (`main:peer/main`), not a single literal head.
    Refspec,
    /// A wildcard pattern, not a literal head.
    Wildcard,
    /// A full ref (`refs/heads/main`), not a head name.
    FullRef,
    /// An explicit remote-tracking ref (`refs/remotes/origin/main`).
    RemoteTracking,
    /// A branch inside the pump's own scratch namespace.
    ScratchNamespace,
    /// The target's own main branch.
    SelfIntake,
    /// A branch already listed.
    Duplicate,
}

impl IntakeIssue {
    /// A short stable category for a validation message.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::NotPlainRef => "not a plain branch name",
            Self::Url => "a git url, not a branch name",
            Self::Refspec => "a refspec, not a literal branch name",
            Self::Wildcard => "a wildcard, not a literal branch name",
            Self::FullRef => "a full ref, not a literal head name",
            Self::RemoteTracking => "a remote-tracking ref, not a literal head name",
            Self::ScratchNamespace => "inside the pump's own scratch namespace",
            Self::SelfIntake => "the target's own main branch",
            Self::Duplicate => "listed twice",
        }
    }
}

/// Why `branch` is not an acceptable intake value, or `None` when it is one.
#[must_use]
pub fn branch_issue(branch: &str, main: &str) -> Option<IntakeIssue> {
    if branch.trim().is_empty() {
        return Some(IntakeIssue::Empty);
    }
    if branch.trim() != branch {
        return Some(IntakeIssue::NotPlainRef);
    }
    if branch.chars().any(|c| matches!(c, '*' | '?' | '[' | ']')) {
        return Some(IntakeIssue::Wildcard);
    }
    if branch.contains("://") || branch.starts_with("git@") {
        return Some(IntakeIssue::Url);
    }
    if branch.starts_with('+')
        || branch.contains(':')
        || branch.contains('^')
        || branch.contains('~')
    {
        return Some(IntakeIssue::Refspec);
    }
    if branch.starts_with("refs/remotes/") || branch.starts_with("remotes/") {
        return Some(IntakeIssue::RemoteTracking);
    }
    if branch.starts_with("refs/") {
        return Some(IntakeIssue::FullRef);
    }
    if branch.starts_with(PUMP_BRANCH_PREFIX) {
        return Some(IntakeIssue::ScratchNamespace);
    }
    if branch == main {
        return Some(IntakeIssue::SelfIntake);
    }
    if !plain_ref(branch) {
        return Some(IntakeIssue::NotPlainRef);
    }
    None
}

/// Every offending value in `branches`, with its position, against `main`.
#[must_use]
pub fn issues(branches: &[String], main: &str) -> Vec<(usize, IntakeIssue)> {
    let mut seen = BTreeSet::new();
    let mut found = Vec::new();
    for (index, branch) in branches.iter().enumerate() {
        if let Some(issue) = branch_issue(branch, main) {
            found.push((index, issue));
        } else if !seen.insert(branch.as_str()) {
            found.push((index, IntakeIssue::Duplicate));
        }
    }
    found
}

#[cfg(test)]
#[path = "intake_test.rs"]
mod tests;
