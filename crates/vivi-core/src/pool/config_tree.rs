//! Optional persistent target trees outside a slot's control directory.

use super::config::PoolConfig;
use super::dispatch::target::JobTarget;
use vivi_error::VivariumError;

impl PoolConfig {
    pub(super) fn check_tree_root(&self, pool: &str) -> Result<(), VivariumError> {
        if let Some(root) = &self.tree_root {
            let valid = root.strip_prefix('/').is_some_and(|rest| {
                !rest.is_empty()
                    && rest.split('/').all(|part| !matches!(part, "" | "." | ".."))
                    && !root.chars().any(char::is_control)
            });
            if !valid {
                return Err(VivariumError::Config(format!(
                    "[pools.{pool}] tree_root must be an absolute directory (not /, no . or .. components or control characters)"
                )));
            }
            if self.workspace.enabled {
                return Err(VivariumError::Config(format!(
                    "[pools.{pool}] tree_root requires persistent directories, not a per-job workspace disk"
                )));
            }
        }
        Ok(())
    }
}

/// Resolve `<root>/<owner>/<repo>/<slot>` from the repository URL, not its
/// target alias or cache key. A local worktree uses its publication URL.
///
/// # Errors
/// Returns an error if a configured tree cannot be named safely from the URL.
pub fn target_tree(
    root: Option<&str>,
    target: &JobTarget,
    slot: &str,
) -> Result<Option<String>, VivariumError> {
    let Some(root) = root else { return Ok(None) };
    let url = if target.local_source.is_some() {
        target.publish_remote.as_deref().unwrap_or(&target.source)
    } else {
        &target.source
    };
    let (owner, repo) = repository_parts(url).ok_or_else(|| {
        VivariumError::Config(format!(
            "tree_root requires an owner/repo repository URL for target {:?}",
            target.id
        ))
    })?;
    if !plain_part(slot) {
        return Err(VivariumError::Config(
            "tree_root received an invalid slot name".into(),
        ));
    }
    Ok(Some(format!("{root}/{owner}/{repo}/{slot}")))
}

fn repository_parts(url: &str) -> Option<(&str, &str)> {
    let path = if let Some((_, rest)) = url.split_once("://") {
        rest.split_once('/')?.1
    } else {
        url.split_once(':')?.1
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.rsplit('/');
    let repo = parts.next()?;
    let owner = parts.next()?;
    (plain_part(owner) && plain_part(repo)).then_some((owner, repo))
}

fn plain_part(part: &str) -> bool {
    !matches!(part, "" | "." | "..")
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
#[path = "config_tree_test.rs"]
mod tests;
