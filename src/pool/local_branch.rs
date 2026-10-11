//! What a dispatch to a worktree slot does with the branch, decided before a
//! slot is claimed.
//!
//! The result of a job in a worktree slot is a commit on `branch` in the
//! local repository. Nothing is ever forced: a branch is never moved, reset
//! or taken from another worktree. The decision table, for `from` and
//! `branch` (after the policy and occupancy refusals below):
//!
//! | `branch` exists | `from` is | Decision |
//! | --- | --- | --- |
//! | no | a commit or another branch | create `branch` at that commit (a stack when `from` is a branch) |
//! | yes | that same branch | continue it: check it out as it is |
//! | yes | anything else | refuse (the local equivalent of a refused non-forced push) |
//!
//! Refused before the table: the repository's default branch, and the branch
//! checked out in its primary checkout (both unless the caller passed the
//! override flag), and any branch another worktree has checked out (Git
//! allows one worktree per branch; the message names the path that holds it).

use std::path::PathBuf;

use super::job::{BranchAction, Checkout, Refs};
use super::local_git::{Repo, Worktree};
use crate::error::VivariumError;

/// Everything the decision reads about the repository.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub repo: PathBuf,
    /// The tip of the local branch being asked for, when it exists.
    pub branch_tip: Option<String>,
    /// The commit `from` names, when it names one.
    pub from_commit: Option<String>,
    pub default_branch: Option<String>,
    /// The branch checked out in the primary checkout.
    pub primary: Option<(PathBuf, String)>,
    /// A worktree that has the branch checked out.
    pub holder: Option<PathBuf>,
    pub allow_default: bool,
}

/// Decide what the job does with `branch`, starting from `from`.
///
/// # Errors
/// Returns the refusal, saying what holds the branch or what to do instead.
pub fn decide(refs: &Refs, facts: &Facts) -> Result<Checkout, VivariumError> {
    let (from, branch) = (refs.from.as_str(), refs.branch.as_str());
    check_policy(branch, facts)?;
    if let Some(path) = &facts.holder {
        return Err(VivariumError::Message(format!(
            "branch \"{branch}\" is checked out in {}; Git allows a branch in one worktree at a time. Free it there (check out something else), or choose another branch",
            path.display()
        )));
    }
    match (&facts.branch_tip, from == branch) {
        (Some(tip), true) => Ok(Checkout {
            action: BranchAction::Continue,
            start: tip.clone(),
        }),
        (Some(_), false) => Err(VivariumError::Message(format!(
            "branch \"{branch}\" already exists and --from is \"{from}\": refusing to move or reset it. Continue it with --from {branch}, or choose a new branch name"
        ))),
        (None, true) => Err(VivariumError::Message(format!(
            "branch \"{branch}\" does not exist in {}, so it cannot be continued; give --from a commit or branch to start it at",
            facts.repo.display()
        ))),
        (None, false) => match &facts.from_commit {
            Some(commit) => Ok(Checkout {
                action: BranchAction::Create,
                start: commit.clone(),
            }),
            None => Err(VivariumError::Message(format!(
                "\"{from}\" is not a commit or branch in {}",
                facts.repo.display()
            ))),
        },
    }
}

fn check_policy(branch: &str, facts: &Facts) -> Result<(), VivariumError> {
    if facts.allow_default {
        return Ok(());
    }
    if facts.default_branch.as_deref() == Some(branch) {
        return Err(VivariumError::Message(format!(
            "refusing to commit to the default branch \"{branch}\" of {}; pass --allow-default-branch if that is intended",
            facts.repo.display()
        )));
    }
    if let Some((path, name)) = &facts.primary
        && name == branch
    {
        return Err(VivariumError::Message(format!(
            "refusing to commit to \"{branch}\", the branch checked out in the primary checkout {}; pass --allow-default-branch if that is intended",
            path.display()
        )));
    }
    Ok(())
}

/// The branch checked out in the primary checkout (the first worktree).
fn primary_of(trees: &[Worktree]) -> Option<(PathBuf, String)> {
    let first = trees.first()?;
    Some((first.path.clone(), first.branch.clone()?))
}

/// The worktree, if any, that has `branch` checked out.
fn holder_of(trees: &[Worktree], branch: &str) -> Option<PathBuf> {
    trees
        .iter()
        .find(|tree| tree.branch.as_deref() == Some(branch))
        .map(|tree| tree.path.clone())
}

/// Read the repository and decide what a job for `refs` does. Read-only.
///
/// # Errors
/// Returns an error if the path is not a Git repository, or the refusal of
/// [`decide`].
pub fn prepare(repo: &Repo, refs: &Refs, allow_default: bool) -> Result<Checkout, VivariumError> {
    let trees = repo.worktrees()?;
    let facts = Facts {
        repo: repo.path().to_path_buf(),
        branch_tip: repo.branch_tip(&refs.branch),
        from_commit: repo.commit_of(&refs.from),
        default_branch: repo.default_branch(),
        primary: primary_of(&trees),
        holder: holder_of(&trees, &refs.branch),
        allow_default,
    };
    decide(refs, &facts)
}

/// The commit a new slot's worktree starts at: the commit `requested` names,
/// else the tip of the default branch, else `HEAD`.
///
/// # Errors
/// Returns an error if the repository has no such commit.
pub fn base_commit(repo: &Repo, requested: Option<&str>) -> Result<String, VivariumError> {
    let rev = match requested {
        Some(rev) => rev.to_string(),
        None => repo
            .default_branch()
            .map_or_else(|| "HEAD".to_string(), |name| format!("refs/heads/{name}")),
    };
    repo.commit_of(&rev).ok_or_else(|| {
        VivariumError::Message(format!(
            "\"{rev}\" is not a commit in {}",
            repo.path().display()
        ))
    })
}

#[cfg(test)]
#[path = "local_branch_test.rs"]
mod tests;
