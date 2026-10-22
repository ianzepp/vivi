//! Extra repository checkouts for a read-only job.
//!
//! A job runs in one repository, but a planner has to read every configured
//! target to plan a unit for any of them. `--also-target <id>` (repeatable,
//! read-only jobs only) adds the named targets' repositories beside the job's
//! own: each is resolved here before any slot is claimed, then the provider
//! prepares it (a worktree slot) or the runner fetches it (every other
//! slot), at `<slot>/targets/<key>/repo`, and the agent's brief names each
//! path. Nothing in an extra checkout is published.

use std::path::Path;

use super::config::PoolConfig;
use super::dispatch::target::{self, JobTarget};
use super::github::{self, Contents, Secret};
use super::job::ExtraCheckout;
use super::local_git::Repo;
use super::plain_git::{self, RepoSource};
use super::progress::Progress;
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;

/// Resolve the named targets into read-only checkouts, in the order given.
/// A target that is the job's own (`primary`), names the same source as it,
/// or is named twice is skipped, so a caller may pass every configured target.
///
/// # Errors
/// Returns an error when a target is unknown, has no source the pool can
/// reach, or its start ref does not resolve to a commit.
pub fn resolve(
    pool_name: &str,
    pool: &PoolConfig,
    pump: &PumpConfig,
    root: &Path,
    primary: Option<&JobTarget>,
    ids: &[String],
    progress: &Progress,
) -> Result<Vec<ExtraCheckout>, VivariumError> {
    let own = own_source(pool, primary);
    let mut seen: Vec<&str> = primary
        .map(|target| target.id.as_str())
        .into_iter()
        .collect();
    let mut extras = Vec::new();
    for id in ids {
        if seen.contains(&id.as_str()) {
            continue;
        }
        seen.push(id);
        let resolved = target::resolve(pool, pump, root, id)?;
        if own.as_deref() == Some(resolved.target.source.as_str()) {
            continue;
        }
        let from = pump
            .for_target(id)
            .map(|config| config.dispatch_from().to_owned())
            .ok_or_else(|| VivariumError::Config(format!("unknown repository target {id:?}")))?;
        progress.say(&format!("resolving {from} of extra target {id}"));
        let (commit, token) = start_of(pool_name, &resolved, &from)?;
        extras.push(ExtraCheckout {
            target: resolved.target,
            from: commit,
            token,
        });
    }
    Ok(extras)
}

/// The source the job's own checkout comes from: its target's, else the
/// pool's own repository.
fn own_source(pool: &PoolConfig, primary: Option<&JobTarget>) -> Option<String> {
    if let Some(target) = primary {
        return Some(target.source.clone());
    }
    let address = pool.remote_address();
    if !address.is_empty() {
        return Some(address);
    }
    let path = pool.worktree_source()?.canonicalize().ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// The commit `from` names in the target's repository, and the read-only
/// token its fetch needs (empty when none is).
fn start_of(
    pool_name: &str,
    resolved: &target::Resolved,
    from: &str,
) -> Result<(String, Secret), VivariumError> {
    if let Some(source) = resolved.target.local_source.as_deref() {
        let commit = Repo::new(Path::new(source))
            .commit_of(from)
            .ok_or_else(|| {
                VivariumError::Message(format!(
                    "cannot resolve \"{from}\" to a commit in {source} for target {}",
                    resolved.target.id
                ))
            })?;
        return Ok((commit, Secret::new(String::new())));
    }
    match resolved.config.require_source(pool_name, "dispatch")? {
        RepoSource::Github(access) => {
            let token = github::mint_token_with(&access, Contents::Read)?;
            let commit = github::resolve_base(&token, access.repo, Some(from))?;
            Ok((commit, token))
        }
        RepoSource::Git(remote) => {
            let commit = plain_git::resolve_base(remote, Some(from))?;
            Ok((commit, Secret::new(String::new())))
        }
    }
}

#[cfg(test)]
#[path = "extras_test.rs"]
mod tests;
