//! Mailspace-owned, repository-bound persistent slots. Paths are operator data,
//! never derived from repository names or pool names.
use super::config::{PoolConfig, bad};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use vivi_error::VivariumError;

/// One `[[slot]]` in the owning mailspace.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SlotConfig {
    pub name: String,
    pub repository: String,
    pub host: String,
    pub dir: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pools: Vec<String>,
}

fn part(value: &str) -> bool {
    !matches!(value, "" | "." | "..")
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Repository identity from the last two components of a Git URL.
#[must_use]
pub fn repository(url: &str) -> Option<String> {
    let path = if let Some((_, rest)) = url.split_once("://") {
        rest.split_once('/')?.1
    } else if let Some((_, path)) = url.split_once(':') {
        path
    } else {
        url
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.rsplit('/');
    let repo = parts.next()?;
    let owner = parts.next()?;
    (part(owner) && part(repo)).then(|| format!("{owner}/{repo}"))
}

/// Validate all declarations before contacting any host.
///
/// # Errors
/// Rejects duplicate names/locations, malformed paths/repositories or routes.
pub fn validate(
    slots: &[SlotConfig],
    pools: &BTreeMap<String, PoolConfig>,
) -> Result<(), VivariumError> {
    let mut names = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for slot in slots {
        if !part(&slot.name) || !names.insert(&slot.name) {
            return Err(bad("[[slot]] name must be unique and a plain name"));
        }
        if repository(&slot.repository).as_deref() != Some(&slot.repository)
            || slot.repository.matches('/').count() != 1
        {
            return Err(bad("[[slot]] repository must be owner/repo (without .git)"));
        }
        let absolute = slot
            .dir
            .strip_prefix('/')
            .is_some_and(|s| !s.is_empty() && s.split('/').all(|p| !matches!(p, "" | "." | "..")));
        if !absolute
            || slot.dir.chars().any(char::is_control)
            || !paths.insert((&slot.host, &slot.dir))
        {
            return Err(bad(
                "[[slot]] dir must be a unique absolute directory (no . or .. components)",
            ));
        }
        super::config::check_ssh_host(&slot.host).map_err(bad)?;
        if slot.pools.iter().any(|p| !pools.contains_key(p)) {
            return Err(bad("[[slot]] pools names an unknown pool"));
        }
    }
    Ok(())
}

/// Bind this pool to its declarations, leaving a counted pool unchanged.
///
/// # Errors
/// Explicit slots need persistent SSH directories, not disposable boundaries.
pub fn route(
    config: &mut PoolConfig,
    pool: &str,
    slots: &[SlotConfig],
) -> Result<(), VivariumError> {
    config.explicit_slots = slots
        .iter()
        .filter(|s| s.pools.is_empty() || s.pools.iter().any(|p| p == pool))
        .cloned()
        .collect();
    if !config.explicit_slots.is_empty() {
        if config.provider != "ssh" || config.workspace.enabled {
            return Err(bad(
                "explicit slots require provider = 'ssh' and persistent directories",
            ));
        }
        config.slots = u32::try_from(config.explicit_slots.len())
            .map_err(|_| bad("too many explicit slots"))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "explicit_test.rs"]
mod tests;
