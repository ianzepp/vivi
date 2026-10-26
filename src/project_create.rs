//! Create, extend and delete projects on a host.
//!
//! One user owns both roots. Project `<name>` lives at
//! `<projects_root>/<name>` and keeps its lanes under `<lanes_root>/<name>/`:
//! one bare, group-shared repository `<target>.git` per pump target, reached
//! at `<git_url_prefix><name>/<target>.git`. A target's checkout is
//! `<projects_root>/<name>/<target>`.
//!
//! [`create`] makes the project's lanes directory, its first target's bare
//! repository, the mailspace (starter `[pump]` included), the first target's
//! checkout and `[pump.targets.<target>]` entry, the project's own values,
//! and the host registration. [`lane_add`] adds another target to an
//! existing project and [`delete`] removes one. Each takes explicit inputs,
//! prints nothing, and returns what it did; the CLI and `vivi-server` both
//! call them.
//!
//! Steps run in order and each is undone when a later one fails. Only paths
//! this call created are removed: a name that already exists is refused up
//! front, and `create_dir` fails on an existing path, so a race cannot make
//! the call delete someone else's work. A rollback failure is reported with
//! the original error.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;
use toml_edit::{Item, value};

use crate::VivariumError;
use crate::mailspace::{MAILSPACE_CONFIG, MAILSPACE_DIR, Mailspace, config_edit};
use crate::pool::config::check_pool_name;
use crate::pool::plain_git::check_address;
use crate::pump::config::valid_target_id;
use crate::pump::readiness;
use crate::registry::{self, Flags};
use crate::routing::{self, Severity};

pub mod delete;
mod lane;
pub mod lane_add;

pub use delete::{ProjectDelete, ProjectDeleted};

const NAME_LIMIT: usize = 64;
const DEFAULT_BRANCH: &str = "main";
/// The pump's integration branch, and the `main` of every target made here:
/// needs land on it, and the upstream branch stays a mirror the pump never
/// writes. The pump creates it on the lane the first time it is missing.
pub const INTEGRATION_BRANCH: &str = "vivi/main";

/// Everything needed to create one project.
#[derive(Clone, Debug)]
pub struct ProjectCreate {
    /// Project name: one safe path segment (see [`check_name`]).
    pub name: String,
    /// Existing directory that holds project roots; the project is
    /// `<projects_root>/<name>`.
    pub projects_root: PathBuf,
    /// Existing directory that holds each project's lanes directory; the
    /// first target's repo is `<lanes_root>/<name>/<target>.git`.
    pub lanes_root: PathBuf,
    /// Lane URLs are `<git_url_prefix><name>/<target>.git`; each must be an
    /// address a plain-git pool accepts (no http or https).
    pub git_url_prefix: String,
    /// The first target's id; `None` uses the project name, which must then
    /// be a valid target id (lowercase letters, digits and hyphens).
    pub target: Option<String>,
    /// Pool whose `remote_url` is set to the first target's URL. The pool
    /// must come from a base config above the projects root. `None` skips
    /// it.
    pub pool: Option<String>,
    /// The first target's default branch: the lane's upstream branch. The
    /// target's own `main` is the pump's integration branch,
    /// [`INTEGRATION_BRANCH`].
    pub branch: String,
    /// Register the project for the pump supervisor.
    pub pump: bool,
    /// Register the project for a server to host.
    pub serve: bool,
    /// Create an empty initial commit on `branch`; otherwise the repo stays
    /// empty until the first push.
    pub initial_commit: bool,
    /// Fill the first target's repository from this Git address (every branch
    /// and tag), fetched by the host. Excludes `initial_commit`; `branch` must
    /// be one of the source's branches.
    pub source: Option<String>,
}

impl ProjectCreate {
    /// A request with the defaults: the target named after the project,
    /// branch `main`, pump and serve on, no pool, no initial commit.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        projects_root: impl Into<PathBuf>,
        lanes_root: impl Into<PathBuf>,
        git_url_prefix: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            projects_root: projects_root.into(),
            lanes_root: lanes_root.into(),
            git_url_prefix: git_url_prefix.into(),
            target: None,
            pool: None,
            branch: DEFAULT_BRANCH.into(),
            pump: true,
            serve: true,
            initial_commit: false,
            source: None,
        }
    }

    /// The first target's id.
    #[must_use]
    pub fn target_id(&self) -> &str {
        self.target.as_deref().unwrap_or(&self.name)
    }
}

/// What [`create`] made.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProjectCreated {
    /// The project root (canonical), which owns `.vivi/`.
    pub project_root: PathBuf,
    /// The project's lanes directory, `<lanes_root>/<name>`.
    pub lanes_dir: PathBuf,
    /// The first target's id.
    pub target: String,
    /// The first target's lane URL, `<git_url_prefix><name>/<target>.git`.
    pub git_url: String,
    /// The first target's bare repository.
    pub repo_path: PathBuf,
    /// The first target's checkout, `<project_root>/<target>`.
    pub checkout_path: PathBuf,
    /// The new mailspace's id.
    pub mailspace_id: String,
    /// Commit created on the branch when `initial_commit` was set.
    pub initial_commit: Option<String>,
    /// The branch tip imported from `source`, when one was given.
    pub imported: Option<String>,
    /// Whether the pump can run the project yet.
    pub readiness: Readiness,
}

/// The pump readiness of a new project.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Readiness {
    /// No errors and nothing missing.
    pub ready: bool,
    /// What the project still needs; the pump holds until each is supplied.
    pub missing: Vec<MissingItem>,
}

/// One item a not-ready project is missing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MissingItem {
    pub item: String,
    pub detail: String,
}

/// The steps of a creation, in order; the failure seam of the tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    Lanes,
    Mailspace,
    Repo,
    Target,
    Values,
    Register,
    Done,
}

/// Create a project, registering it in the host registry
/// (`VIVI_CONFIG_DIR` or the user config directory).
///
/// # Errors
/// Fails on invalid input, a name that already exists (lanes directory,
/// project root or registration), or any step failing; everything the call
/// created is then removed, and a failed removal is named in the error.
pub fn create(request: &ProjectCreate) -> Result<ProjectCreated, VivariumError> {
    create_at(request, &registry::path()?)
}

/// [`create`] against an explicit registry file.
///
/// # Errors
/// As [`create`].
pub fn create_at(
    request: &ProjectCreate,
    registry_file: &Path,
) -> Result<ProjectCreated, VivariumError> {
    execute(request, registry_file, &|_| Ok(()))
}

fn execute(
    request: &ProjectCreate,
    registry_file: &Path,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
) -> Result<ProjectCreated, VivariumError> {
    let plan = Plan::of(request)?;
    refuse_existing(&plan.lanes, "lanes directory")?;
    refuse_existing(&plan.root, "project root")?;
    refuse_registered(&plan.root, registry_file)?;
    let mut undo = Undo::default();
    match run_steps(request, &plan, registry_file, inject, &mut undo) {
        Ok(created) => Ok(created),
        Err(error) => Err(undo.roll_back(error, &plan, registry_file)),
    }
}

/// Validated inputs and the paths they name.
struct Plan {
    root: PathBuf,
    lanes: PathBuf,
    target: String,
    repo: PathBuf,
    git_url: String,
}

impl Plan {
    fn of(request: &ProjectCreate) -> Result<Self, VivariumError> {
        check_name(&request.name)?;
        let target = request.target_id().to_string();
        if !valid_target_id(&target) {
            return Err(config(&format!(
                "target id {target:?} must use lowercase letters, digits and hyphens; name the \
                 first target explicitly when the project name is not one"
            )));
        }
        check_branch(&request.branch)?;
        if request.initial_commit && request.source.is_some() {
            return Err(config(
                "a project made from a source has its history; drop the initial commit",
            ));
        }
        if let Some(pool) = &request.pool {
            check_pool_name(pool)?;
        }
        let git_url = lane_url(&request.git_url_prefix, &request.name, &target);
        check_address(request.pool.as_deref().unwrap_or("pool"), &git_url)?;
        let projects = existing_dir(&request.projects_root, "projects root")?;
        let lanes_root = existing_dir(&request.lanes_root, "lanes root")?;
        let lanes = lanes_root.join(&request.name);
        Ok(Self {
            root: projects.join(&request.name),
            repo: lanes.join(format!("{target}.git")),
            lanes,
            target,
            git_url,
        })
    }
}

/// `<prefix><project>/<target>.git`.
#[must_use]
pub fn lane_url(prefix: &str, project: &str, target: &str) -> String {
    format!("{prefix}{project}/{target}.git")
}

fn refuse_registered(root: &Path, registry_file: &Path) -> Result<(), VivariumError> {
    if registry::load_at(registry_file)?.get(root).is_some() {
        return Err(config(&format!("{} is already registered", root.display())));
    }
    Ok(())
}

/// What this call has created so far.
#[derive(Default)]
struct Undo {
    lanes: bool,
    root: bool,
    registered: bool,
}

impl Undo {
    /// Remove what was created, newest first. Returns the original error
    /// with every rollback failure appended.
    fn roll_back(self, error: VivariumError, plan: &Plan, registry_file: &Path) -> VivariumError {
        let mut failures = Vec::new();
        if self.registered {
            let removed = registry::update_at(registry_file, |registry| {
                registry.remove(&plan.root);
                Ok(())
            });
            if let Err(failure) = removed {
                failures.push(format!(
                    "could not unregister {}: {failure}",
                    plan.root.display()
                ));
            }
        }
        for (made, path) in [(self.root, &plan.root), (self.lanes, &plan.lanes)] {
            if made && let Err(failure) = fs::remove_dir_all(path) {
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

fn run_steps(
    request: &ProjectCreate,
    plan: &Plan,
    registry_file: &Path,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
    undo: &mut Undo,
) -> Result<ProjectCreated, VivariumError> {
    inject(Step::Lanes)?;
    fs::create_dir(&plan.lanes)?;
    undo.lanes = true;

    inject(Step::Mailspace)?;
    fs::create_dir(&plan.root)?;
    undo.root = true;
    Mailspace::init_with_defaults(Some(&plan.root))?;

    inject(Step::Repo)?;
    let initial_commit = lane::init_bare(
        &plan.repo,
        &request.branch,
        request.initial_commit,
        &plan.root,
    )?;
    let imported = match &request.source {
        Some(source) => Some(lane::import(&plan.repo, source, &request.branch)?),
        None => None,
    };

    inject(Step::Target)?;
    let attached = lane_add::attach_target(
        &plan.root,
        &plan.target,
        &plan.repo,
        &plan.git_url,
        Some(INTEGRATION_BRANCH),
    )?;

    inject(Step::Values)?;
    write_values(request, &plan.root, &plan.git_url)?;
    let mailspace = Mailspace::open_root(plan.root.clone())?;

    inject(Step::Register)?;
    let issues = readiness::assess(&mailspace);
    undo.registered = register(request, &plan.root, registry_file, &issues)?;
    inject(Step::Done)?;

    Ok(ProjectCreated {
        project_root: plan.root.clone(),
        lanes_dir: plan.lanes.clone(),
        target: plan.target.clone(),
        git_url: plan.git_url.clone(),
        repo_path: plan.repo.clone(),
        checkout_path: attached.checkout_path,
        mailspace_id: mailspace.config.id.clone().unwrap_or_default(),
        initial_commit,
        imported,
        readiness: summarize(&issues),
    })
}

/// Add the project to the registry. A project with a pump must have no
/// wrong values (missing items are fine, as for `pump service add`). Returns
/// true when this call added the entry.
fn register(
    request: &ProjectCreate,
    root: &Path,
    registry_file: &Path,
    issues: &[routing::Issue],
) -> Result<bool, VivariumError> {
    if request.pump {
        routing::require_ok(issues)?;
    }
    let flags = Flags {
        pump: request.pump,
        serve: request.serve,
    };
    registry::update_at(registry_file, |registry| {
        if registry.add(root.to_path_buf(), flags) {
            Ok(true)
        } else {
            Err(config(&format!("{} is already registered", root.display())))
        }
    })
}

/// Write the pool's `remote_url` into the project's own config file.
fn write_values(request: &ProjectCreate, root: &Path, git_url: &str) -> Result<(), VivariumError> {
    let Some(pool) = &request.pool else {
        return Ok(());
    };
    let mailspace = Mailspace::open_root(root.to_path_buf())?;
    if !mailspace.config.pools.contains_key(pool) {
        return Err(config(&format!(
            "pool `{pool}` is not defined by a base config above {}; a project's own \
             [pools.{pool}] only overrides remote_url on an inherited pool",
            root.display()
        )));
    }
    let path = root.join(MAILSPACE_DIR).join(MAILSPACE_CONFIG);
    config_edit::apply(&path, |doc| {
        let table = config_edit::table_at(doc.as_table_mut(), &["pools", pool])?;
        table["remote_url"] = value(git_url);
        if let Some(pools) = doc.get_mut("pools").and_then(Item::as_table_mut) {
            pools.set_implicit(true);
        }
        Ok(())
    })?;
    Ok(())
}

/// Readiness of the project at `root` as the pump sees it now.
///
/// # Errors
/// Fails when the mailspace cannot be opened.
pub fn readiness_of(root: &Path) -> Result<Readiness, VivariumError> {
    let mailspace = Mailspace::open_root(root.to_path_buf())?;
    Ok(summarize(&readiness::assess(&mailspace)))
}

fn summarize(issues: &[routing::Issue]) -> Readiness {
    let missing = issues
        .iter()
        .filter(|issue| issue.severity == Severity::NotReady)
        .map(|issue| MissingItem {
            item: issue.item.clone().unwrap_or_default(),
            detail: issue.message.clone(),
        })
        .collect();
    Readiness {
        ready: readiness::is_ready(issues),
        missing,
    }
}

/// A git invocation; output is captured and never printed.
struct Git(Command);

fn git(repo: Option<&Path>, args: &[&str]) -> Git {
    let mut command = Command::new("git");
    if let Some(repo) = repo {
        command.arg("-C").arg(repo);
    }
    command.args(args).stdin(Stdio::null());
    Git(command)
}

impl Git {
    fn arg(mut self, arg: &Path) -> Self {
        self.0.arg(arg);
        self
    }

    fn env(mut self, key: &str, value: &str) -> Self {
        self.0.env(key, value);
        self
    }

    /// Run and return trimmed stdout.
    fn run(mut self) -> Result<String, VivariumError> {
        let output = self.0.output()?;
        if !output.status.success() {
            return Err(VivariumError::Other(format!(
                "git {} failed: {}",
                self.0
                    .get_args()
                    .map(|arg| arg.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

fn config(message: &str) -> VivariumError {
    VivariumError::Config(message.to_string())
}

fn existing_dir(path: &Path, what: &str) -> Result<PathBuf, VivariumError> {
    match path.canonicalize() {
        Ok(dir) if dir.is_dir() => Ok(dir),
        _ => Err(config(&format!(
            "{what} {} is not an existing directory",
            path.display()
        ))),
    }
}

/// Refuse anything at `path`, a dangling symlink included.
fn refuse_existing(path: &Path, what: &str) -> Result<(), VivariumError> {
    if path.symlink_metadata().is_ok() {
        return Err(config(&format!("{what} {} already exists", path.display())));
    }
    Ok(())
}

/// A project name is one safe path segment: lowercase letters, digits, `-`,
/// `.` and `_`; it starts with a letter or digit, is at most 64 characters,
/// and does not end in `.git` or `.`.
///
/// # Errors
/// Returns a configuration error naming the rule that failed.
pub fn check_name(name: &str) -> Result<(), VivariumError> {
    let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || "-._".contains(c);
    let starts_well = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if name.is_empty() || name.len() > NAME_LIMIT || !starts_well || !name.chars().all(allowed) {
        return Err(config(&format!(
            "project name {name:?} must be 1-{NAME_LIMIT} characters of lowercase letters, digits, \
             `-`, `.` and `_`, starting with a letter or digit"
        )));
    }
    if name.rsplit_once('.').is_some_and(|(_, ext)| ext == "git") || name.ends_with('.') {
        return Err(config(&format!(
            "project name {name:?} must not end in `.git` or `.`"
        )));
    }
    Ok(())
}

/// A conservative branch name: letters, digits and `-._/`, not starting with
/// `-`, `.` or `/`, with no `..`, `//`, `/.` and no trailing `/`, `.` or
/// `.lock`.
fn check_branch(branch: &str) -> Result<(), VivariumError> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || "-._/".contains(c);
    let bad = branch.is_empty()
        || !branch.chars().all(allowed)
        || branch.starts_with(['-', '.', '/'])
        || branch.ends_with(['/', '.'])
        || branch
            .rsplit_once('.')
            .is_some_and(|(_, ext)| ext == "lock")
        || ["..", "//", "/."].iter().any(|part| branch.contains(part));
    if bad {
        return Err(config(&format!(
            "branch {branch:?} is not a usable branch name"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "project_create_test.rs"]
mod tests;
