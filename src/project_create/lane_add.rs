//! Attach a lane to an existing project, in halves a host can split across
//! users.
//!
//! [`add_split`] adds one repository target to a project that already
//! exists. The halves are the same two a host can split across users:
//!
//! - the **lanes half**: the group-shared bare repository
//!   `<lanes_root>/<target-id>.git` with the optional seed commit, the same
//!   [`create_lane`](super::create_lane) performs for a new project;
//! - the **projects half**: inside the project, a clone of the repository at
//!   the target's checkout path (`<project_root>/<target-id>`, which becomes
//!   the `[pump.targets.<id>]` `path`) and the `[pump.targets.<id>]` entry
//!   written through the guarded config edit, so a running pump refuses.
//!
//! [`add_split`] probes both roots, performs every half the caller can
//! write, and reports the exact `vivi project lane add` command for the user
//! who owns the rest. A second add for the same target id is refused: the
//! lane repository, the checkout or the target entry already exists, and
//! each is refused before anything new is made. Only paths this call created
//! are undone when a later step fails; a repository another user's run made
//! is never removed.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{
    Half, LaneCreate, LaneCreated, RemainingHalf, config, create_lane, existing_dir, git,
    refuse_existing, shell_arg, writable_root,
};
use crate::VivariumError;
use crate::mailspace::Mailspace;
use crate::pump::config::targets::{self, TargetSpec};
use crate::pump::config::valid_target_id;

/// Everything needed to attach one lane.
#[derive(Clone, Debug)]
pub struct LaneAdd {
    /// Existing project root that owns `.vivi/`.
    pub project_root: PathBuf,
    /// Target id: the `[pump.targets.<id>]` key; the lane is
    /// `<lanes_root>/<target-id>.git` and the checkout is
    /// `<project_root>/<target-id>` (lowercase letters, digits and hyphens).
    pub target_id: String,
    /// Existing directory that holds bare repositories.
    pub lanes_root: PathBuf,
    /// The git URL is `<git_url_prefix><target-id>.git`; it must be an
    /// address a plain-git pool accepts (no http or https), because the
    /// checkout's remote is also where pool jobs push. `None` clones from
    /// the repository's local path and writes no `remote_url`: the
    /// checkout's `origin` remote names the lane.
    pub git_url_prefix: Option<String>,
    /// Create the seed commit on the lane's branch (lanes half only).
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

/// What the projects half of a lane add made.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TargetAttached {
    /// The id written under `[pump.targets]`.
    pub target_id: String,
    /// The clone of the lane at the target's `path`.
    pub checkout_path: PathBuf,
    /// The git URL the clone came from (the lane's local path when no
    /// `git_url_prefix` was given).
    pub git_url: String,
}

/// What a lane add performed, and what remains for another user.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LaneAdded {
    /// The lanes half's result when this run performed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<LaneCreated>,
    /// The projects half's result when this run performed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetAttached>,
    /// The half this run could not write.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining: Option<RemainingHalf>,
}

/// The steps of a lane add, in order; the failure seam of the tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    /// Before the lanes half starts.
    Lane,
    /// Before the checkout is cloned.
    Clone,
    /// Before the target entry is written.
    Target,
}

/// Attach a lane by halves: probe both roots, perform every half the caller
/// can write, and report the exact command for the user who owns the rest.
///
/// # Errors
/// Fails on an invalid target id or git URL, a missing root, a half failing
/// (everything this call created is then removed), or an error naming both
/// roots when the caller can write neither.
pub fn add_split(request: &LaneAdd) -> Result<LaneAdded, VivariumError> {
    add_split_with(request, &|_| Ok(()))
}

fn add_split_with(
    request: &LaneAdd,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
) -> Result<LaneAdded, VivariumError> {
    check_id(&request.target_id)?;
    let lanes_dir = existing_dir(&request.lanes_root, "lanes root")?;
    let project_dir = existing_dir(&request.project_root, "project root")?;
    let url = lane_url(request, &lanes_dir);
    check_url(&url)?;
    let repo = lanes_dir.join(format!("{}.git", request.target_id));
    let checkout = project_dir.join(&request.target_id);
    let lanes_writable = writable_root(&lanes_dir)?;
    let projects_writable = writable_root(&project_dir)?;
    match (lanes_writable, projects_writable) {
        (true, true) => both(
            request,
            &lanes_dir,
            &project_dir,
            &repo,
            &checkout,
            &url,
            inject,
        ),
        (true, false) => lanes_only(request, &lanes_dir, &project_dir, &repo, inject),
        (false, true) => projects_only(
            request,
            &repo,
            &checkout,
            &project_dir,
            &lanes_dir,
            &url,
            inject,
        ),
        (false, false) => Err(config(&format!(
            "no half can be created: no write permission on the lanes root {} or the project \
             root {}",
            lanes_dir.display(),
            project_dir.display()
        ))),
    }
}

/// Both halves in one call, with one rollback.
fn both(
    request: &LaneAdd,
    lanes_dir: &Path,
    project_dir: &Path,
    repo: &Path,
    checkout: &Path,
    url: &str,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
) -> Result<LaneAdded, VivariumError> {
    let mut made = Made::default();
    let lane = match run_lane_half(request, lanes_dir, project_dir, inject) {
        Ok(lane) => lane,
        // create_lane undoes its own repository.
        Err(error) => return Err(made.roll_back(error)),
    };
    made.push(repo.to_path_buf());
    match run_target_steps(request, project_dir, repo, checkout, url, inject, &mut made) {
        Ok(target) => Ok(LaneAdded {
            lane: Some(lane),
            target: Some(target),
            remaining: None,
        }),
        Err(error) => Err(made.roll_back(error)),
    }
}

/// The lanes half alone, plus the command for the projects-half owner.
fn lanes_only(
    request: &LaneAdd,
    lanes_dir: &Path,
    project_dir: &Path,
    repo: &Path,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
) -> Result<LaneAdded, VivariumError> {
    let lane = run_lane_half(request, lanes_dir, project_dir, inject)?;
    Ok(LaneAdded {
        lane: Some(lane),
        target: None,
        remaining: Some(RemainingHalf {
            half: Half::Projects,
            root: project_dir.to_path_buf(),
            command: remainder_command(request),
        }),
    })
}

/// The projects half alone, plus the command for the lanes-half owner. The
/// lane repository is never touched, so it may already exist (the lanes
/// owner's run made it); it must exist, or there is nothing to clone.
fn projects_only(
    request: &LaneAdd,
    repo: &Path,
    checkout: &Path,
    project_dir: &Path,
    lanes_dir: &Path,
    url: &str,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
) -> Result<LaneAdded, VivariumError> {
    let mut made = Made::default();
    let target =
        match run_target_steps(request, project_dir, repo, checkout, url, inject, &mut made) {
            Ok(target) => target,
            Err(error) => return Err(made.roll_back(error)),
        };
    Ok(LaneAdded {
        lane: None,
        target: Some(target),
        remaining: Some(RemainingHalf {
            half: Half::Lanes,
            root: lanes_dir.to_path_buf(),
            command: remainder_command(request),
        }),
    })
}

/// The lanes half: the bare repository and the optional seed commit.
fn run_lane_half(
    request: &LaneAdd,
    lanes_dir: &Path,
    project_dir: &Path,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
) -> Result<LaneCreated, VivariumError> {
    inject(Step::Lane)?;
    let mut lane = LaneCreate::new(&request.target_id, lanes_dir, project_dir);
    lane.initial_commit = request.seed;
    create_lane(&lane)
}

/// The projects half: the checkout at the target's `path` and the
/// `[pump.targets.<id>]` entry, written through the guarded config edit.
fn run_target_steps(
    request: &LaneAdd,
    project_dir: &Path,
    repo: &Path,
    checkout: &Path,
    url: &str,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
    made: &mut Made,
) -> Result<TargetAttached, VivariumError> {
    let mailspace = Mailspace::open_root(project_dir.to_path_buf())?;
    refuse_existing_target(&mailspace, &request.target_id)?;
    refuse_existing(checkout, "target checkout")?;
    if !repo.is_dir() {
        return Err(config(&format!(
            "lane repository {} does not exist; the lanes half must run first",
            repo.display()
        )));
    }
    inject(Step::Clone)?;
    made.push(checkout.to_path_buf());
    git(None, &["clone", url]).arg(checkout).run()?;
    inject(Step::Target)?;
    targets::add(project_dir, &target_spec(request, url))?;
    Ok(TargetAttached {
        target_id: request.target_id.clone(),
        checkout_path: checkout.to_path_buf(),
        git_url: url.to_string(),
    })
}

/// A target id that already exists is refused before anything is made, the
/// same refusal `pump::config::targets::add` makes when it writes.
fn refuse_existing_target(mailspace: &Mailspace, id: &str) -> Result<(), VivariumError> {
    if mailspace
        .config
        .pump
        .as_ref()
        .is_some_and(|pump| pump.targets.contains_key(id))
    {
        return Err(config(&format!("pump target `{id}` already exists")));
    }
    Ok(())
}

fn target_spec(request: &LaneAdd, url: &str) -> TargetSpec {
    TargetSpec {
        id: request.target_id.clone(),
        path: request.target_id.clone(),
        remote_url: request.git_url_prefix.is_some().then(|| url.to_string()),
        ..TargetSpec::default()
    }
}

/// The lane's git URL: the prefix form when given, else the repository's
/// local path.
fn lane_url(request: &LaneAdd, lanes_dir: &Path) -> String {
    match &request.git_url_prefix {
        Some(prefix) => format!("{}{}.git", prefix, request.target_id),
        None => lanes_dir
            .join(format!("{}.git", request.target_id))
            .display()
            .to_string(),
    }
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

/// The exact `vivi project lane add` command that performs the remaining
/// half when run by a user who can write its root, with the same inputs.
fn remainder_command(request: &LaneAdd) -> String {
    let mut command = format!(
        "vivi project lane add {} {} --lanes-root {}",
        shell_arg(&request.project_root.display().to_string()),
        shell_arg(&request.target_id),
        shell_arg(&request.lanes_root.display().to_string()),
    );
    if let Some(prefix) = &request.git_url_prefix {
        command.push_str(&format!(" --git-url-prefix {}", shell_arg(prefix)));
    }
    if request.seed {
        command.push_str(" --seed");
    }
    command
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
