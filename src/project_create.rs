//! Create a project on a host in one operation.
//!
//! [`create`] makes a group-shared bare git repository, a project directory
//! with an initialised mailspace (starter `[pump]` included), writes the
//! project's own values, and registers the project with the host registry.
//! It takes explicit inputs, prints nothing, and returns a
//! [`ProjectCreated`]; the CLI and `vivi-server` both call it.
//!
//! Steps run in order and each is undone when a later one fails. Only paths
//! this call created are removed: a name that already exists on either side
//! is refused up front, and the directories are made with `create_dir`, which
//! fails on an existing path, so a race cannot make the call delete someone
//! else's work. A rollback failure is reported with the original error.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;
use toml_edit::{Item, value};

use crate::VivariumError;
use crate::mailspace::{MAILSPACE_CONFIG, MAILSPACE_DIR, Mailspace, config_edit};
use crate::pool::config::check_pool_name;
use crate::pool::plain_git::check_address;
use crate::pump::readiness;
use crate::registry::{self, Flags};
use crate::routing::{self, Severity};

const NAME_LIMIT: usize = 64;
const DEFAULT_BRANCH: &str = "main";
/// Author and committer of the initial commit when the project's `[pump]`
/// names no committer.
const FALLBACK_IDENTITY: (&str, &str) = ("vivi", "vivi@localhost");

/// Everything needed to create one project.
#[derive(Clone, Debug)]
pub struct ProjectCreate {
    /// Project name: one safe path segment (see [`check_name`]).
    pub name: String,
    /// Existing directory that holds project roots; the project is
    /// `<projects_root>/<name>`.
    pub projects_root: PathBuf,
    /// Existing directory that holds bare repositories; the repo is
    /// `<lanes_root>/<name>.git`.
    pub lanes_root: PathBuf,
    /// The git URL is `<git_url_prefix><name>.git`; it must be an address a
    /// plain-git pool accepts (no http or https).
    pub git_url_prefix: String,
    /// Pool whose `remote_url` is set to the git URL. The pool must come from
    /// a base config above the projects root. `None` skips it.
    pub pool: Option<String>,
    /// The new repo's default branch.
    pub branch: String,
    /// Register the project for the pump supervisor.
    pub pump: bool,
    /// Register the project for a server to host.
    pub serve: bool,
    /// Create an empty initial commit on `branch`; otherwise the repo stays
    /// empty until the first push.
    pub initial_commit: bool,
}

impl ProjectCreate {
    /// A request with the defaults: branch `main`, pump and serve on, no
    /// pool, no gate, no initial commit.
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
            pool: None,
            branch: DEFAULT_BRANCH.into(),
            pump: true,
            serve: true,
            initial_commit: false,
        }
    }
}

/// What [`create`] made.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProjectCreated {
    /// The project root (canonical), which owns `.vivi/`.
    pub project_root: PathBuf,
    /// `<git_url_prefix><name>.git`.
    pub git_url: String,
    /// The bare repository (canonical parent).
    pub repo_path: PathBuf,
    /// The new mailspace's id.
    pub mailspace_id: String,
    /// Commit created on the branch when `initial_commit` was set.
    pub initial_commit: Option<String>,
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
    Repo,
    Mailspace,
    Values,
    Commit,
    Register,
    Done,
}

/// Create a project, registering it in the host registry
/// (`VIVI_CONFIG_DIR` or the user config directory).
///
/// # Errors
/// Fails on invalid input, a name that already exists (repo, project root or
/// registration), or any step failing; everything the call created is then
/// removed, and a failed removal is named in the error.
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
    let plan = Plan::validate(request, registry_file)?;
    let mut undo = Undo::default();
    match run_steps(request, &plan, registry_file, inject, &mut undo) {
        Ok(created) => Ok(created),
        Err(error) => Err(undo.roll_back(error, &plan, registry_file)),
    }
}

/// Validated paths and URL.
struct Plan {
    root: PathBuf,
    repo: PathBuf,
    git_url: String,
}

impl Plan {
    fn validate(request: &ProjectCreate, registry_file: &Path) -> Result<Self, VivariumError> {
        check_name(&request.name)?;
        check_branch(&request.branch)?;
        if let Some(pool) = &request.pool {
            check_pool_name(pool)?;
        }
        let git_url = format!("{}{}.git", request.git_url_prefix, request.name);
        check_address(request.pool.as_deref().unwrap_or("pool"), &git_url)?;
        let root = existing_dir(&request.projects_root, "projects root")?.join(&request.name);
        let repo =
            existing_dir(&request.lanes_root, "lanes root")?.join(format!("{}.git", request.name));
        refuse_existing(&root, "project root")?;
        refuse_existing(&repo, "repository")?;
        if registry::load_at(registry_file)?.get(&root).is_some() {
            return Err(config(&format!("{} is already registered", root.display())));
        }
        Ok(Self {
            root,
            repo,
            git_url,
        })
    }
}

/// What this call has created so far.
#[derive(Default)]
struct Undo {
    repo: bool,
    root: bool,
    registered: bool,
}

impl Undo {
    /// Remove what was created, newest first. Returns the original error, or
    /// the original error with every rollback failure appended.
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
        for (made, path) in [(self.root, &plan.root), (self.repo, &plan.repo)] {
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
    inject(Step::Repo)?;
    fs::create_dir(&plan.repo)?;
    undo.repo = true;
    git(
        None,
        &["init", "--bare", "--shared=group", "-b", &request.branch],
    )
    .arg(&plan.repo)
    .run()?;

    inject(Step::Mailspace)?;
    fs::create_dir(&plan.root)?;
    undo.root = true;
    Mailspace::init_with_defaults(Some(&plan.root))?;

    inject(Step::Values)?;
    write_values(request, plan)?;
    let mailspace = Mailspace::open_root(plan.root.clone())?;

    inject(Step::Commit)?;
    let initial_commit = if request.initial_commit {
        Some(make_initial_commit(
            &plan.repo,
            &request.branch,
            &mailspace,
        )?)
    } else {
        None
    };

    inject(Step::Register)?;
    let issues = readiness::assess(&mailspace);
    undo.registered = register(request, plan, registry_file, &issues)?;
    inject(Step::Done)?;

    Ok(ProjectCreated {
        project_root: plan.root.clone(),
        git_url: plan.git_url.clone(),
        repo_path: plan.repo.clone(),
        mailspace_id: mailspace.config.id.clone().unwrap_or_default(),
        initial_commit,
        readiness: summarize(&issues),
    })
}

/// Add the project to the registry. A project with a pump must have no
/// wrong values (missing items are fine, as for `pump service add`). Returns
/// true when this call added the entry.
fn register(
    request: &ProjectCreate,
    plan: &Plan,
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
        if registry.add(plan.root.clone(), flags) {
            Ok(true)
        } else {
            Err(config(&format!(
                "{} is already registered",
                plan.root.display()
            )))
        }
    })
}

/// Write the pool's `remote_url` into the project's own config file.
fn write_values(request: &ProjectCreate, plan: &Plan) -> Result<(), VivariumError> {
    let path = plan.root.join(MAILSPACE_DIR).join(MAILSPACE_CONFIG);
    if let Some(pool) = &request.pool {
        let mailspace = Mailspace::open_root(plan.root.clone())?;
        if !mailspace.config.pools.contains_key(pool) {
            return Err(config(&format!(
                "pool `{pool}` is not defined by a base config above {}; a project's own \
                 [pools.{pool}] only overrides remote_url on an inherited pool",
                plan.root.display()
            )));
        }
    }
    config_edit::apply(&path, |doc| {
        if let Some(pool) = &request.pool {
            let table = config_edit::table_at(doc.as_table_mut(), &["pools", pool])?;
            table["remote_url"] = value(&plan.git_url);
            if let Some(pools) = doc.get_mut("pools").and_then(Item::as_table_mut) {
                pools.set_implicit(true);
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// An empty commit on `refs/heads/<branch>` of the bare repo, authored by the
/// project's `[pump]` committer when both parts are set, else `vivi
/// <vivi@localhost>`. Returns the commit id.
fn make_initial_commit(
    repo: &Path,
    branch: &str,
    mailspace: &Mailspace,
) -> Result<String, VivariumError> {
    let (name, email) = mailspace
        .config
        .pump
        .as_ref()
        .filter(|pump| !pump.committer_name.is_empty() && !pump.committer_email.is_empty())
        .map_or(FALLBACK_IDENTITY, |pump| {
            (pump.committer_name.as_str(), pump.committer_email.as_str())
        });
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
/// and does not end in `.git` (the repo adds that itself).
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
