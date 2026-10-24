//! The lanes half of a creation: the bare group-shared repository.
//!
//! [`create_lane`] makes `<lanes_root>/<name>.git` with `--shared=group`
//! and the optional seed commit. A host can own the lanes root with a
//! different user than the projects root, so the half is public on its own;
//! the seed commit's identity comes from the config chain at the identity
//! root, which is what a new mailspace inherits.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{
    Git, Step, Undo, check_branch, check_name, config, existing_dir, git, refuse_existing,
};
use crate::VivariumError;
use crate::mailspace::{MAILSPACE_CONFIG, MAILSPACE_DIR, inherit};

/// Author and committer of the initial commit when the project's `[pump]`
/// names no committer.
const FALLBACK_IDENTITY: (&str, &str) = ("vivi", "vivi@localhost");

/// The lanes half's input: the bare repository under the lanes root.
#[derive(Clone, Debug)]
pub struct LaneCreate {
    /// Project name: the repository is `<lanes_root>/<name>.git`.
    pub name: String,
    /// Existing directory that holds bare repositories.
    pub lanes_root: PathBuf,
    /// Root whose config chain names the seed commit's identity (the
    /// project's `[pump]` committer, else the vivi fallback).
    pub identity_root: PathBuf,
    /// The new repo's default branch.
    pub branch: String,
    /// Create the seed commit on `branch`.
    pub initial_commit: bool,
}

impl LaneCreate {
    /// A lanes-half request with the defaults: branch `main`, no seed commit.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        lanes_root: impl Into<PathBuf>,
        identity_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            name: name.into(),
            lanes_root: lanes_root.into(),
            identity_root: identity_root.into(),
            branch: super::DEFAULT_BRANCH.into(),
            initial_commit: false,
        }
    }
}

/// What the lanes half made.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LaneCreated {
    /// The bare repository (canonical parent).
    pub repo_path: PathBuf,
    /// Commit created on the branch when `initial_commit` was set.
    pub initial_commit: Option<String>,
}

/// The lanes half of a project creation, alone: a group-shared bare
/// repository with the optional seed commit, for a user who can write the
/// lanes root while another owns the projects half.
///
/// # Errors
/// Fails on an invalid name or branch, a missing lanes root, a repository
/// that already exists, or git failing; the repository is then removed.
pub fn create_lane(request: &LaneCreate) -> Result<LaneCreated, VivariumError> {
    check_name(&request.name)?;
    check_branch(&request.branch)?;
    let lanes_dir = existing_dir(&request.lanes_root, "lanes root")?;
    let repo = lanes_dir.join(format!("{}.git", request.name));
    refuse_existing(&repo, "repository")?;
    let plan = LanePlan {
        repo: repo.clone(),
        branch: request.branch.clone(),
        initial_commit: request.initial_commit,
        identity_root: request.identity_root.clone(),
    };
    let mut undo = Undo::default();
    match run_lane_steps(&plan, &|_| Ok(()), &mut undo) {
        Ok(lane) => Ok(lane),
        Err(error) => Err(undo.roll_back(error, Some(&repo), None)),
    }
}

/// Validated lanes-half inputs.
pub(super) struct LanePlan {
    pub repo: PathBuf,
    pub branch: String,
    pub initial_commit: bool,
    pub identity_root: PathBuf,
}

/// The lanes half: the bare, group-shared repository and its optional seed
/// commit. `undo.repo` is set once the directory exists.
pub(super) fn run_lane_steps(
    plan: &LanePlan,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
    undo: &mut Undo,
) -> Result<LaneCreated, VivariumError> {
    inject(Step::Repo)?;
    fs::create_dir(&plan.repo)?;
    undo.repo = true;
    git(
        None,
        &["init", "--bare", "--shared=group", "-b", &plan.branch],
    )
    .arg(&plan.repo)
    .run()?;
    // W3E3-3 copies a sibling repository's group and ACL entries here.
    inject(Step::Commit)?;
    let initial_commit = if plan.initial_commit {
        let identity = resolve_committer(&plan.identity_root)?;
        Some(make_initial_commit(&plan.repo, &plan.branch, &identity)?)
    } else {
        None
    };
    Ok(LaneCreated {
        repo_path: plan.repo.clone(),
        initial_commit,
    })
}

/// The seed commit's author and committer: the `[pump]` committer of the
/// config chain at `root` — its own file when it exists, else the bases
/// above it, which is what a new mailspace inherits — when both parts are
/// set, else the vivi fallback. An unreadable chain falls back quietly, as
/// config inheritance does.
fn resolve_committer(root: &Path) -> Result<(String, String), VivariumError> {
    let path = root.join(MAILSPACE_DIR).join(MAILSPACE_CONFIG);
    let raw = match fs::read_to_string(&path) {
        Ok(text) => text,
        // The project does not exist yet: stand in for the `name`-only file
        // `Mailspace::init` will write, so the base chain merges as it will
        // after creation.
        Err(_) => format!(
            "name = {:?}\n",
            root.file_name().unwrap_or_default().to_string_lossy()
        ),
    };
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
