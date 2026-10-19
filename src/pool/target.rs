//! Resolve a workspace repository without changing pool capacity or identity.

use std::path::Path;

use sha2::{Digest, Sha256};

use super::super::config::PoolConfig;
use crate::VivariumError;
use crate::pump::config::PumpConfig;

/// Repository selection delivered with one job. Cache names contain no paths.
#[derive(Debug, Clone)]
pub struct JobTarget {
    pub id: String,
    pub source: String,
    /// Repository's configured push/fetch URL for local privileged publication.
    pub publish_remote: Option<String>,
    pub cache_key: String,
    pub local_source: Option<String>,
    pub seed: Vec<String>,
    pub requires: Vec<String>,
    pub build_dir: Option<String>,
}

/// One resolved target plus the pool's settings with its legacy source replaced.
pub struct Resolved {
    pub config: PoolConfig,
    pub target: JobTarget,
}

/// Resolve only configured targets; unknown targets never fall back to pool.repo.
pub fn resolve(
    pool: &PoolConfig,
    pump: &PumpConfig,
    root: &Path,
    id: &str,
) -> Result<Resolved, VivariumError> {
    let target = pump.target_configs().remove(id).ok_or_else(|| {
        VivariumError::Config(format!("unknown workspace repository target {id:?}"))
    })?;
    let path = pump
        .target_path(root, id)
        .ok_or_else(|| VivariumError::Config(format!("repository target {id:?} has no path")))?;
    let publish_remote = target
        .remote_url
        .clone()
        .or_else(|| super::super::local_git::Repo::new(&path).remote_url(&target.remote));
    let mut config = pool.clone();
    config.embedded = None;
    config.repo = None;
    config.local.worktree_of = None;
    let local =
        pool.provider == "local" && (pool.worktree_source().is_some() || pool.github.is_none());
    let source = if local {
        let path = if pump.targets.is_empty() {
            pool.worktree_source().unwrap_or(path)
        } else {
            pump.target_repo_dir(root, id).ok_or_else(|| {
                VivariumError::Config(format!("target {id:?} has no integration checkout"))
            })?
        };
        let path = path.canonicalize()?;
        super::super::local_git::Repo::new(&path).common_dir()?;
        config.local.worktree_of = Some(path.to_string_lossy().into_owned());
        path.to_string_lossy().into_owned()
    } else {
        resolve_remote(pool, &mut config, pump, &target, &path)?
    };
    let cache_key = hex::encode(Sha256::digest(format!("{id}\0{source}")));
    Ok(Resolved {
        target: JobTarget {
            id: id.to_string(),
            source,
            publish_remote,
            cache_key,
            local_source: config.local.worktree_of.clone(),
            seed: config.seed.clone(),
            requires: config.requires.clone(),
            build_dir: config.build_dir.clone(),
        },
        config,
    })
}

fn resolve_remote(
    pool: &PoolConfig,
    config: &mut PoolConfig,
    pump: &PumpConfig,
    target: &crate::pump::config::PumpTarget,
    path: &Path,
) -> Result<String, VivariumError> {
    let remote = target
        .remote_url
        .clone()
        .or_else(|| super::super::local_git::Repo::new(path).remote_url(&target.remote));
    let inferred = remote.as_deref().and_then(github_repository);
    let github = target
        .github_repo
        .clone()
        .or(inferred.clone())
        .or_else(|| pump.targets.is_empty().then(|| pool.repo.clone()).flatten());
    if let (Some(explicit), Some(inferred)) = (&target.github_repo, &inferred)
        && explicit != inferred
    {
        return Err(VivariumError::Config(
            "target github_repo does not match its remote_url repository".into(),
        ));
    }
    let repo = github.ok_or_else(|| {
        VivariumError::Config("remote target requires github_repo or a GitHub remote_url".into())
    })?;
    if !pump.targets.is_empty() && remote.is_some() && inferred.is_none() {
        return Err(VivariumError::Config(
            "remote target source must match its GitHub repository".into(),
        ));
    }
    if github_repository(&format!("https://github.com/{repo}.git")).as_deref()
        != Some(repo.as_str())
    {
        return Err(VivariumError::Config(
            "target github_repo must be owner/name".into(),
        ));
    }
    config.repo = Some(repo);
    config.require_repo("workspace", "dispatch target")?;
    Ok(config.remote_address())
}

fn github_repository(remote: &str) -> Option<String> {
    let name = remote
        .strip_prefix("https://github.com/")
        .or_else(|| remote.strip_prefix("git@github.com:"))
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))?
        .trim_end_matches('/');
    let name = name.strip_suffix(".git").unwrap_or(name);
    let mut parts = name.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    };
    (parts.next().is_none() && valid(owner) && valid(repo)).then(|| name.into())
}
