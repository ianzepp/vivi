//! A lane: one bare, group-shared repository under a project's lanes
//! directory.
//!
//! [`init_bare`] makes `<lanes_root>/<project>/<target>.git` with
//! `--shared=group` and the optional seed commit. Group and ACL come from
//! the lanes directory itself (a setgid directory with default ACLs hands
//! both to everything made inside it), so nothing is copied here. The seed
//! commit's identity comes from the config chain at the project root.

use std::fs;
use std::path::Path;

use super::git;
use crate::VivariumError;
use crate::mailspace::{MAILSPACE_CONFIG, MAILSPACE_DIR, inherit};

/// Author and committer of the initial commit when the project's `[pump]`
/// names no committer.
const FALLBACK_IDENTITY: (&str, &str) = ("vivi", "vivi@localhost");

/// Make the bare repository `repo` on `branch` and, when `seed` is set, one
/// empty commit on it. Returns the seed commit's id. The caller owns undo:
/// `git init` creates `repo`, so a failure after it leaves `repo` for the
/// caller to remove.
pub(super) fn init_bare(
    repo: &Path,
    branch: &str,
    seed: bool,
    identity_root: &Path,
) -> Result<Option<String>, VivariumError> {
    git(None, &["init", "--bare", "--shared=group", "-b", branch])
        .arg(repo)
        .run()?;
    if !seed {
        return Ok(None);
    }
    let identity = resolve_committer(identity_root)?;
    Ok(Some(make_initial_commit(repo, branch, &identity)?))
}

/// The seed commit's author and committer: the `[pump]` committer of the
/// config chain at `root` when both parts are set, else the vivi fallback.
/// An unreadable chain falls back quietly, as config inheritance does.
fn resolve_committer(root: &Path) -> Result<(String, String), VivariumError> {
    let path = root.join(MAILSPACE_DIR).join(MAILSPACE_CONFIG);
    let raw = fs::read_to_string(&path).unwrap_or_default();
    let config = inherit::load_config(&path, &raw)?;
    let pump = config
        .pump
        .as_ref()
        .filter(|pump| !pump.committer_name.is_empty() && !pump.committer_email.is_empty());
    Ok(match pump {
        Some(pump) => (pump.committer_name.clone(), pump.committer_email.clone()),
        None => (FALLBACK_IDENTITY.0.into(), FALLBACK_IDENTITY.1.into()),
    })
}

/// An empty commit on `refs/heads/<branch>` of the bare repo, authored by
/// the given identity. Returns the commit id.
fn make_initial_commit(
    repo: &Path,
    branch: &str,
    identity: &(String, String),
) -> Result<String, VivariumError> {
    let (name, email) = identity;
    let tree = git(Some(repo), &["mktree"]).run()?;
    let commit = git(Some(repo), &["commit-tree", &tree, "-m", "Initial commit"])
        .env("GIT_AUTHOR_NAME", name)
        .env("GIT_AUTHOR_EMAIL", email)
        .env("GIT_COMMITTER_NAME", name)
        .env("GIT_COMMITTER_EMAIL", email)
        .run()?;
    git(
        Some(repo),
        &["update-ref", &format!("refs/heads/{branch}"), &commit],
    )
    .run()?;
    Ok(commit)
}
