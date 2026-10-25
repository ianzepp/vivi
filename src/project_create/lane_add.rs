//! Add a lane to an existing project.
//!
//! [`add`] makes one more pump target: the bare, group-shared repository
//! `<lanes_root>/<project>/<target>.git` (and the project's lanes directory
//! when it is missing), a clone of it at `<project_root>/<target>`, and the
//! `[pump.targets.<target>]` entry written through the guarded config edit,
//! so a running pump refuses (`vivi pump pause` first). `<project>` is the
//! project root's directory name. A second add for the same target id is
//! refused: the repository, the checkout or the target entry already exists,
//! and each is refused before anything new is made. Only paths this call
//! created are undone when a later step fails.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{config, existing_dir, git, lane, refuse_existing};
use crate::VivariumError;
use crate::mailspace::Mailspace;
use crate::pump::config::targets::{self, TargetSpec};
use crate::pump::config::valid_target_id;

/// Everything needed to add one lane.
#[derive(Clone, Debug)]
pub struct LaneAdd {
    /// Existing project root that owns `.vivi/`.
    pub project_root: PathBuf,
    /// Target id: the `[pump.targets.<id>]` key, the lane
    /// `<lanes_root>/<project>/<id>.git` and the checkout
    /// `<project_root>/<id>` (lowercase letters, digits and hyphens).
    pub target_id: String,
    /// Existing directory that holds each project's lanes directory.
    pub lanes_root: PathBuf,
    /// The lane URL is `<git_url_prefix><project>/<id>.git`; it must be an
    /// address a plain-git pool accepts (no http or https), because the
    /// checkout's remote is also where pool jobs push. `None` clones from
    /// the repository's local path and writes no `remote_url`: the
    /// checkout's `origin` remote names the lane.
    pub git_url_prefix: Option<String>,
    /// Create the seed commit on the lane's branch.
    pub seed: bool,
}

impl LaneAdd {
    /// A lane-add request with the defaults: no git URL prefix, no seed
    /// commit.
    #[must_use]
    pub fn new(
        project_root: impl Into<PathBuf>,
        target_id: impl Into<String>,
        lanes_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            project_root: project_root.into(),
            target_id: target_id.into(),
            lanes_root: lanes_root.into(),
            git_url_prefix: None,
            seed: false,
        }
    }
}

/// What [`add`] made.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LaneAdded {
    /// The id written under `[pump.targets]`.
    pub target_id: String,
    /// The bare repository.
    pub repo_path: PathBuf,
    /// The clone of the lane at the target's `path`.
    pub checkout_path: PathBuf,
    /// The git URL the clone came from (the lane's local path when no
    /// `git_url_prefix` was given).
    pub git_url: String,
    /// Commit created on the branch when `seed` was set.
    pub initial_commit: Option<String>,
}

/// What [`attach_target`] made.
pub(super) struct TargetAttached {
    pub checkout_path: PathBuf,
}

/// The steps of a lane add, in order; the failure seam of the tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    Lane,
    Target,
}

/// Add one lane to an existing project.
///
/// # Errors
/// Fails on an invalid target id or git URL, a missing root, an existing
/// repository, checkout or target, or any step failing; everything this
/// call created is then removed.
pub fn add(request: &LaneAdd) -> Result<LaneAdded, VivariumError> {
    add_with(request, &|_| Ok(()))
}

fn add_with(
    request: &LaneAdd,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
) -> Result<LaneAdded, VivariumError> {
    check_id(&request.target_id)?;
    let lanes_root = existing_dir(&request.lanes_root, "lanes root")?;
    let project_dir = existing_dir(&request.project_root, "project root")?;
    let project = project_name(&project_dir)?;
    let lanes_dir = lanes_root.join(&project);
    let repo = lanes_dir.join(format!("{}.git", request.target_id));
    let url = match &request.git_url_prefix {
        Some(prefix) => super::lane_url(prefix, &project, &request.target_id),
        None => repo.display().to_string(),
    };
    check_url(&url)?;
    refuse_existing(&repo, "lane repository")?;
    refuse_existing_target(&project_dir, &request.target_id)?;
    let mut made = Made::default();
    match run_steps(
        request,
        &project_dir,
        &lanes_dir,
        &repo,
        &url,
        inject,
        &mut made,
    ) {
        Ok(added) => Ok(added),
        Err(error) => Err(made.roll_back(error)),
    }
}

fn run_steps(
    request: &LaneAdd,
    project_dir: &Path,
    lanes_dir: &Path,
    repo: &Path,
    url: &str,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
    made: &mut Made,
) -> Result<LaneAdded, VivariumError> {
    inject(Step::Lane)?;
    if lanes_dir.symlink_metadata().is_err() {
        fs::create_dir(lanes_dir)?;
        made.push(lanes_dir.to_path_buf());
    }
    made.push(repo.to_path_buf());
    let branch = super::DEFAULT_BRANCH;
    let initial_commit = lane::init_bare(repo, branch, request.seed, project_dir)?;
    inject(Step::Target)?;
    made.push(project_dir.join(&request.target_id));
    let remote_url = request.git_url_prefix.as_ref().map(|_| url);
    let attached = attach(project_dir, &request.target_id, repo, url, remote_url, None)?;
    Ok(LaneAdded {
        target_id: request.target_id.clone(),
        repo_path: repo.to_path_buf(),
        checkout_path: attached.checkout_path,
        git_url: url.to_string(),
        initial_commit,
    })
}

/// Clone the lane `repo` to `<project_dir>/<target>` with `url` as its
/// `origin`, and write the `[pump.targets.<target>]` entry with `url` as its
/// `remote_url` and `main` as its main branch when given. The caller owns
/// undo of the checkout.
pub(super) fn attach_target(
    project_dir: &Path,
    target: &str,
    repo: &Path,
    url: &str,
    main: Option<&str>,
) -> Result<TargetAttached, VivariumError> {
    attach(project_dir, target, repo, url, Some(url), main)
}

/// The clone reads the lane from its local path, so making a checkout never
/// needs the network; `origin` is then pointed at `url`, which is where
/// pool jobs and the pump push.
fn attach(
    project_dir: &Path,
    target: &str,
    repo: &Path,
    url: &str,
    remote_url: Option<&str>,
    main: Option<&str>,
) -> Result<TargetAttached, VivariumError> {
    let checkout = project_dir.join(target);
    refuse_existing(&checkout, "target checkout")?;
    git(None, &["clone", "--quiet"])
        .arg(repo)
        .arg(&checkout)
        .run()?;
    git(Some(&checkout), &["remote", "set-url", "origin", url]).run()?;
    let spec = TargetSpec {
        id: target.to_string(),
        path: target.to_string(),
        remote_url: remote_url.map(str::to_string),
        main: main.map(str::to_string),
        ..TargetSpec::default()
    };
    targets::add(project_dir, &spec)?;
    Ok(TargetAttached {
        checkout_path: checkout,
    })
}

/// The project's name: its root directory's name.
fn project_name(project_dir: &Path) -> Result<String, VivariumError> {
    project_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .ok_or_else(|| {
            config(&format!(
                "project root {} has no usable directory name",
                project_dir.display()
            ))
        })
}

/// A target id that already exists is refused before anything is made, the
/// same refusal `pump::config::targets::add` makes when it writes.
fn refuse_existing_target(project_dir: &Path, id: &str) -> Result<(), VivariumError> {
    let mailspace = Mailspace::open_root(project_dir.to_path_buf())?;
    if mailspace
        .config
        .pump
        .as_ref()
        .is_some_and(|pump| pump.targets.contains_key(id))
    {
        return Err(config(&format!("pump target `{id}` already exists")));
    }
    refuse_existing(&project_dir.join(id), "target checkout")
}

/// A target id must be a valid `[pump.targets.<id>]` key, or the guarded
/// write that adds the target would be rolled back as an error.
fn check_id(id: &str) -> Result<(), VivariumError> {
    if !valid_target_id(id) {
        return Err(config(&format!(
            "target id {id:?} must use lowercase letters, digits and hyphens"
        )));
    }
    Ok(())
}

/// The lane's git URL must be an address Git reaches without a token.
fn check_url(url: &str) -> Result<(), VivariumError> {
    if url.starts_with("https://") || url.starts_with("http://") {
        return Err(config(&format!(
            "lane git url {url} is an http(s) address; use an SSH or file:// address for a \
             plain Git remote"
        )));
    }
    if url.is_empty() || url.starts_with('-') || url.chars().any(char::is_whitespace) {
        return Err(config(&format!(
            "lane git url {url:?} is not a Git address"
        )));
    }
    Ok(())
}

/// What this call has created so far, in creation order.
#[derive(Default)]
struct Made {
    paths: Vec<PathBuf>,
}

impl Made {
    fn push(&mut self, path: PathBuf) {
        self.paths.push(path);
    }

    /// Remove what was created, newest first. A path that is already gone
    /// (a failed clone may have cleaned up after itself) is fine. Returns
    /// the original error with every rollback failure appended.
    fn roll_back(self, error: VivariumError) -> VivariumError {
        let mut failures = Vec::new();
        for path in self.paths.iter().rev() {
            if let Err(failure) = fs::remove_dir_all(path)
                && failure.kind() != ErrorKind::NotFound
            {
                failures.push(format!("could not remove {}: {failure}", path.display()));
            }
        }
        if failures.is_empty() {
            error
        } else {
            VivariumError::Other(format!(
                "{error}; rollback also failed: {}",
                failures.join("; ")
            ))
        }
    }
}

#[cfg(test)]
#[path = "lane_add_test.rs"]
mod tests;
