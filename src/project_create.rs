//! Create a project on a host, in halves a host can split across users.
//!
//! [`create`] makes a group-shared bare git repository, a project directory
//! with an initialised mailspace (starter `[pump]` included), writes the
//! project's own values, and registers the project with the host registry.
//! It takes explicit inputs, prints nothing, and returns a
//! [`ProjectCreated`]; the CLI and `vivi-server` both call it.
//!
//! The work is two halves, because a host can own the two roots with two
//! users (one for the lanes root, one for the projects root):
//!
//! - the **lanes half** ([`lane::create_lane`]): the bare repository, the
//!   sibling group and ACL copy, and its optional seed commit;
//! - the **projects half** ([`create_project_half`]): the mailspace, the
//!   project's own values, and the host registration.
//!
//! [`create_split_at`] probes both roots, performs every half the caller can
//! write, and reports the exact `vivi project create` command for the user
//! who owns the rest. Both halves share the input validation.
//! [`lane_add`] does the same for one more lane on a project that already
//! exists.
//!
//! Steps run in order and each is undone when a later one fails. Only paths
//! this call created are removed: a name that already exists is refused up
//! front, and `create_dir` fails on an existing path, so a race cannot make
//! the call delete someone else's work. A half that was never started is
//! never rolled back. A rollback failure is reported with the original error.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use toml_edit::{Item, value};

use crate::VivariumError;
use crate::mailspace::{MAILSPACE_CONFIG, MAILSPACE_DIR, Mailspace, config_edit};
use crate::pool::config::check_pool_name;
use crate::pool::plain_git::check_address;
use crate::pump::readiness;
use crate::registry::{self, Flags};
use crate::routing::{self, Severity};

mod lane;
pub mod lane_add;

pub use lane::{LaneCreate, LaneCreated, create_lane};
use lane::{LanePlan, run_lane_steps};

const NAME_LIMIT: usize = 64;
const DEFAULT_BRANCH: &str = "main";

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

/// What the projects half made.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProjectHalfCreated {
    /// The project root (canonical), which owns `.vivi/`.
    pub project_root: PathBuf,
    /// `<git_url_prefix><name>.git`.
    pub git_url: String,
    /// The new mailspace's id.
    pub mailspace_id: String,
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

/// One half of a creation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Half {
    /// The bare repository under the lanes root.
    Lanes,
    /// The mailspace, values and registration under the projects root.
    Projects,
}

/// The half of a split operation this caller could not perform.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RemainingHalf {
    /// Which half remains.
    pub half: Half,
    /// The root the performing user must be able to write.
    pub root: PathBuf,
    /// The exact `vivi` command for the remaining half; run by a user who
    /// can write its root, it performs that half by the same probes.
    pub command: String,
}

/// What a split creation performed, and what remains for another user.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SplitCreated {
    /// The lanes half's result when this run performed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<LaneCreated>,
    /// The projects half's result when this run performed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectHalfCreated>,
    /// The half this run could not write.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining: Option<RemainingHalf>,
}

impl SplitCreated {
    /// A both-halves outcome from a full [`create_at`] result.
    fn of(created: ProjectCreated) -> Self {
        Self {
            lane: Some(LaneCreated {
                repo_path: created.repo_path,
                initial_commit: created.initial_commit,
            }),
            project: Some(ProjectHalfCreated {
                project_root: created.project_root,
                git_url: created.git_url,
                mailspace_id: created.mailspace_id,
                readiness: created.readiness,
            }),
            remaining: None,
        }
    }

    /// The both-halves result when this run performed everything.
    #[must_use]
    pub fn full(&self) -> Option<ProjectCreated> {
        match (&self.lane, &self.project, self.remaining.as_ref()) {
            (Some(lane), Some(project), None) => Some(ProjectCreated {
                project_root: project.project_root.clone(),
                git_url: project.git_url.clone(),
                repo_path: lane.repo_path.clone(),
                mailspace_id: project.mailspace_id.clone(),
                initial_commit: lane.initial_commit.clone(),
                readiness: project.readiness.clone(),
            }),
            _ => None,
        }
    }
}

/// The steps of a creation, in order; the failure seam of the tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    Repo,
    Commit,
    Mailspace,
    Values,
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

/// The projects half of [`create`], alone: the mailspace with the starter
/// `[pump]`, the project's own values, and the host registration. A user who
/// can write the projects root runs this when another user owns the lanes
/// half; the repository itself is never touched, so it may already exist.
///
/// # Errors
/// Fails on invalid input, a name that already exists (project root or
/// registration), or any step failing; everything this call created is then
/// removed, and a failed removal is named in the error.
pub fn create_project_half(
    request: &ProjectCreate,
    registry_file: &Path,
) -> Result<ProjectHalfCreated, VivariumError> {
    let git_url = validate_shared(request)?;
    let projects_dir = existing_dir(&request.projects_root, "projects root")?;
    let root = projects_dir.join(&request.name);
    refuse_existing(&root, "project root")?;
    refuse_registered(&root, registry_file)?;
    let mut undo = Undo::default();
    match run_project_steps(
        request,
        &root,
        &git_url,
        registry_file,
        &|_| Ok(()),
        &mut undo,
    ) {
        Ok(created) => Ok(created),
        Err(error) => Err(undo.roll_back(error, None, Some((&root, registry_file)))),
    }
}

/// [`create`] by halves against the host registry (`VIVI_CONFIG_DIR` or the
/// user config directory); see [`create_split_at`].
///
/// # Errors
/// As [`create_split_at`], plus a registry path that cannot be determined.
pub fn create_split(request: &ProjectCreate) -> Result<SplitCreated, VivariumError> {
    create_split_at(request, &registry::path()?)
}

/// [`create`] split across owners: probe both roots, perform every half the
/// caller can write, and report the exact command for the user who owns the
/// rest. When the caller can write both roots this is exactly [`create_at`].
///
/// # Errors
/// As [`create_at`] for the halves performed, plus an error naming both
/// roots when the caller can write neither.
pub fn create_split_at(
    request: &ProjectCreate,
    registry_file: &Path,
) -> Result<SplitCreated, VivariumError> {
    validate_shared(request)?;
    let dirs = Roots::of(request)?;
    let lanes_writable = writable_root(&dirs.lanes)?;
    let projects_writable = writable_root(&dirs.projects)?;
    split_execute(
        request,
        registry_file,
        &dirs,
        &|_| Ok(()),
        lanes_writable,
        projects_writable,
    )
}

fn execute(
    request: &ProjectCreate,
    registry_file: &Path,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
) -> Result<ProjectCreated, VivariumError> {
    let git_url = validate_shared(request)?;
    let dirs = Roots::of(request)?;
    run_all(request, registry_file, &dirs, &git_url, inject)
}

/// Perform every half the caller can write; the flags come from real probes
/// (see [`writable_root`]) or a test.
fn split_execute(
    request: &ProjectCreate,
    registry_file: &Path,
    dirs: &Roots,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
    lanes_writable: bool,
    projects_writable: bool,
) -> Result<SplitCreated, VivariumError> {
    let git_url = validate_shared(request)?;
    match (lanes_writable, projects_writable) {
        (true, true) => Ok(SplitCreated::of(run_all(
            request,
            registry_file,
            dirs,
            &git_url,
            inject,
        )?)),
        (true, false) => lanes_only(request, dirs, inject),
        (false, true) => projects_only(request, registry_file, dirs, &git_url, inject),
        (false, false) => Err(config(&format!(
            "no half can be created: no write permission on the lanes root {} or the projects \
             root {}",
            dirs.lanes.display(),
            dirs.projects.display()
        ))),
    }
}

/// Both halves in one call, with one shared validation and one rollback.
fn run_all(
    request: &ProjectCreate,
    registry_file: &Path,
    dirs: &Roots,
    git_url: &str,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
) -> Result<ProjectCreated, VivariumError> {
    let root = dirs.projects.join(&request.name);
    let repo = dirs.lanes.join(format!("{}.git", request.name));
    refuse_existing(&root, "project root")?;
    refuse_existing(&repo, "repository")?;
    refuse_registered(&root, registry_file)?;
    let plan = Plan {
        root,
        repo,
        git_url: git_url.to_string(),
    };
    let mut undo = Undo::default();
    match run_all_steps(request, &plan, registry_file, inject, &mut undo) {
        Ok(created) => Ok(created),
        Err(error) => {
            Err(undo.roll_back(error, Some(&plan.repo), Some((&plan.root, registry_file))))
        }
    }
}

/// The lanes half alone, plus the command for the projects-half owner.
fn lanes_only(
    request: &ProjectCreate,
    dirs: &Roots,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
) -> Result<SplitCreated, VivariumError> {
    let repo = dirs.lanes.join(format!("{}.git", request.name));
    refuse_existing(&repo, "repository")?;
    let plan = LanePlan {
        repo: repo.clone(),
        branch: request.branch.clone(),
        initial_commit: request.initial_commit,
        identity_root: dirs.projects.join(&request.name),
    };
    let mut undo = Undo::default();
    let lane = match run_lane_steps(&plan, inject, &mut undo) {
        Ok(lane) => lane,
        Err(error) => return Err(undo.roll_back(error, Some(&repo), None)),
    };
    Ok(SplitCreated {
        lane: Some(lane),
        project: None,
        remaining: Some(RemainingHalf {
            half: Half::Projects,
            root: dirs.projects.clone(),
            command: remainder_command(request, dirs),
        }),
    })
}

/// The projects half alone, plus the command for the lanes-half owner.
fn projects_only(
    request: &ProjectCreate,
    registry_file: &Path,
    dirs: &Roots,
    git_url: &str,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
) -> Result<SplitCreated, VivariumError> {
    let root = dirs.projects.join(&request.name);
    refuse_existing(&root, "project root")?;
    refuse_registered(&root, registry_file)?;
    let mut undo = Undo::default();
    let project = match run_project_steps(request, &root, git_url, registry_file, inject, &mut undo)
    {
        Ok(created) => created,
        Err(error) => return Err(undo.roll_back(error, None, Some((&root, registry_file)))),
    };
    Ok(SplitCreated {
        lane: None,
        project: Some(project),
        remaining: Some(RemainingHalf {
            half: Half::Lanes,
            root: dirs.lanes.clone(),
            command: remainder_command(request, dirs),
        }),
    })
}

/// Validated paths and URL.
struct Plan {
    root: PathBuf,
    repo: PathBuf,
    git_url: String,
}

/// The canonical roots both halves live under.
struct Roots {
    projects: PathBuf,
    lanes: PathBuf,
}

impl Roots {
    fn of(request: &ProjectCreate) -> Result<Self, VivariumError> {
        Ok(Self {
            projects: existing_dir(&request.projects_root, "projects root")?,
            lanes: existing_dir(&request.lanes_root, "lanes root")?,
        })
    }
}

/// The checks every half shares: the name, the branch, the pool name and the
/// git URL. Returns the git URL.
fn validate_shared(request: &ProjectCreate) -> Result<String, VivariumError> {
    check_name(&request.name)?;
    check_branch(&request.branch)?;
    if let Some(pool) = &request.pool {
        check_pool_name(pool)?;
    }
    let git_url = format!("{}{}.git", request.git_url_prefix, request.name);
    check_address(request.pool.as_deref().unwrap_or("pool"), &git_url)?;
    Ok(git_url)
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
    repo: bool,
    root: bool,
    registered: bool,
}

impl Undo {
    /// Remove what was created, newest first; a half that never ran (its
    /// path is `None`) is never rolled back. Returns the original error with
    /// every rollback failure appended.
    fn roll_back(
        self,
        error: VivariumError,
        repo: Option<&Path>,
        projects: Option<(&Path, &Path)>,
    ) -> VivariumError {
        let mut failures = Vec::new();
        if let Some((root, registry_file)) = projects {
            if self.registered {
                let removed = registry::update_at(registry_file, |registry| {
                    registry.remove(root);
                    Ok(())
                });
                if let Err(failure) = removed {
                    failures.push(format!(
                        "could not unregister {}: {failure}",
                        root.display()
                    ));
                }
            }
            if self.root
                && let Err(failure) = fs::remove_dir_all(root)
            {
                failures.push(format!("could not remove {}: {failure}", root.display()));
            }
        }
        if let Some(repo) = repo
            && self.repo
            && let Err(failure) = fs::remove_dir_all(repo)
        {
            failures.push(format!("could not remove {}: {failure}", repo.display()));
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

fn run_all_steps(
    request: &ProjectCreate,
    plan: &Plan,
    registry_file: &Path,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
    undo: &mut Undo,
) -> Result<ProjectCreated, VivariumError> {
    let lane_plan = LanePlan {
        repo: plan.repo.clone(),
        branch: request.branch.clone(),
        initial_commit: request.initial_commit,
        identity_root: plan.root.clone(),
    };
    let lane = run_lane_steps(&lane_plan, inject, undo)?;
    let project = run_project_steps(
        request,
        &plan.root,
        &plan.git_url,
        registry_file,
        inject,
        undo,
    )?;
    Ok(ProjectCreated {
        project_root: plan.root.clone(),
        git_url: plan.git_url.clone(),
        repo_path: lane.repo_path,
        mailspace_id: project.mailspace_id,
        initial_commit: lane.initial_commit,
        readiness: project.readiness,
    })
}

/// The projects half: the mailspace with the starter `[pump]`, the project's
/// own values, and the host registration. `undo.root` and
/// `undo.registered` are set as each is created.
fn run_project_steps(
    request: &ProjectCreate,
    root: &Path,
    git_url: &str,
    registry_file: &Path,
    inject: &dyn Fn(Step) -> Result<(), VivariumError>,
    undo: &mut Undo,
) -> Result<ProjectHalfCreated, VivariumError> {
    inject(Step::Mailspace)?;
    fs::create_dir(root)?;
    undo.root = true;
    Mailspace::init_with_defaults(Some(root))?;

    inject(Step::Values)?;
    write_values(request, root, git_url)?;
    let mailspace = Mailspace::open_root(root.to_path_buf())?;

    inject(Step::Register)?;
    let issues = readiness::assess(&mailspace);
    undo.registered = register(request, root, registry_file, &issues)?;
    inject(Step::Done)?;

    Ok(ProjectHalfCreated {
        project_root: root.to_path_buf(),
        git_url: git_url.to_string(),
        mailspace_id: mailspace.config.id.clone().unwrap_or_default(),
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
    let path = root.join(MAILSPACE_DIR).join(MAILSPACE_CONFIG);
    if let Some(pool) = &request.pool {
        let mailspace = Mailspace::open_root(root.to_path_buf())?;
        if !mailspace.config.pools.contains_key(pool) {
            return Err(config(&format!(
                "pool `{pool}` is not defined by a base config above {}; a project's own \
                 [pools.{pool}] only overrides remote_url on an inherited pool",
                root.display()
            )));
        }
    }
    config_edit::apply(&path, |doc| {
        if let Some(pool) = &request.pool {
            let table = config_edit::table_at(doc.as_table_mut(), &["pools", pool])?;
            table["remote_url"] = value(git_url);
            if let Some(pools) = doc.get_mut("pools").and_then(Item::as_table_mut) {
                pools.set_implicit(true);
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// The exact `vivi project create` command that performs the remaining half
/// when run by a user who can write its root, with the same inputs.
fn remainder_command(request: &ProjectCreate, dirs: &Roots) -> String {
    let mut command = format!(
        "vivi project create {} --projects-root {} --lanes-root {} --git-url-prefix {}",
        shell_arg(&request.name),
        shell_arg(&dirs.projects.display().to_string()),
        shell_arg(&dirs.lanes.display().to_string()),
        shell_arg(&request.git_url_prefix),
    );
    if let Some(pool) = &request.pool {
        command.push_str(&format!(" --pool {}", shell_arg(pool)));
    }
    if request.branch != DEFAULT_BRANCH {
        command.push_str(&format!(" --branch {}", shell_arg(&request.branch)));
    }
    if !request.pump {
        command.push_str(" --no-pump");
    }
    if !request.serve {
        command.push_str(" --no-serve");
    }
    if request.initial_commit {
        command.push_str(" --initial-commit");
    }
    command
}

/// Quote one shell word unless every character is safe unquoted.
fn shell_arg(arg: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./:=@+%".contains(c);
    if !arg.is_empty() && arg.chars().all(safe) {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

/// Whether this process can create entries in `dir`, probed by making and
/// removing one throwaway directory. A permission denial means no; any other
/// failure is an error.
fn writable_root(dir: &Path) -> Result<bool, VivariumError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    let probe = dir.join(format!(".vivi-probe-{}-{nanos}", std::process::id()));
    match fs::create_dir(&probe) {
        Ok(()) => {
            fs::remove_dir(&probe)?;
            Ok(true)
        }
        Err(error) if error.kind() == ErrorKind::PermissionDenied => Ok(false),
        Err(error) => Err(error.into()),
    }
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
