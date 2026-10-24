//! Tests for [`super`] (`project_create::lane_add`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::{TempDir, tempdir};

use super::{Half, LaneAdd, Step, add_split_with};
use crate::VivariumError;
use crate::project_create::{LaneCreate, ProjectCreate, create_at, create_lane};
use crate::pump::config::targets;
use crate::pump::store::{DB_FILENAME, PumpStore};

struct Site {
    _tmp: TempDir,
    projects: PathBuf,
    lanes: PathBuf,
    registry: PathBuf,
}

impl Site {
    fn new() -> Self {
        let tmp = tempdir().unwrap();
        let projects = tmp.path().join("projects");
        let lanes = tmp.path().join("lanes");
        fs::create_dir(&projects).unwrap();
        fs::create_dir(&lanes).unwrap();
        let registry = tmp.path().join("config/vivi/pump-service.toml");
        let (projects, lanes) = (
            projects.canonicalize().unwrap(),
            lanes.canonicalize().unwrap(),
        );
        Self {
            _tmp: tmp,
            projects,
            lanes,
            registry,
        }
    }

    /// A project made the normal way; its root is returned.
    fn project(&self, name: &str) -> PathBuf {
        let request = ProjectCreate::new(name, &self.projects, &self.lanes, "git@lanes:");
        create_at(&request, &self.registry).unwrap();
        self.projects.join(name)
    }

    fn request(&self, project: &Path, id: &str) -> LaneAdd {
        LaneAdd::new(project, id, &self.lanes)
    }

    /// A request whose lane URL is a real, local `file://` address.
    fn prefixed(&self, project: &Path, id: &str) -> LaneAdd {
        let mut request = self.request(project, id);
        request.git_url_prefix = Some(self.prefix());
        request
    }

    fn prefix(&self) -> String {
        format!("file://{}/", self.lanes.display())
    }

    fn repo(&self, id: &str) -> PathBuf {
        self.lanes.join(format!("{id}.git"))
    }

    fn checkout(&self, project: &Path, id: &str) -> PathBuf {
        project.join(id)
    }

    /// The lane repository another user's lanes half would have made.
    fn make_lane(&self, project: &Path, id: &str, seed: bool) {
        let mut lane = LaneCreate::new(id, &self.lanes, project);
        lane.initial_commit = seed;
        create_lane(&lane).unwrap();
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn config_text(project: &Path) -> String {
    fs::read_to_string(project.join(".vivi/mailspace.toml")).unwrap()
}

fn target_ids(project: &Path) -> Vec<String> {
    targets::list(project)
        .unwrap()
        .into_iter()
        .map(|row| row.id)
        .collect()
}

fn fail_at(step: Step) -> impl Fn(Step) -> Result<(), VivariumError> {
    move |current| {
        if current == step {
            Err(VivariumError::Other("injected failure".into()))
        } else {
            Ok(())
        }
    }
}

#[cfg(unix)]
fn deny_writes(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o555)).unwrap();
}

#[cfg(unix)]
fn allow_writes(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn both_halves_make_the_repo_the_checkout_and_the_target() {
    let site = Site::new();
    let project = site.project("app");
    let added = add_split_with(&site.prefixed(&project, "sandbox"), &|_| Ok(())).unwrap();

    assert!(added.remaining.is_none(), "{added:?}");
    assert_eq!(
        git(
            &site.repo("sandbox"),
            &["rev-parse", "--is-bare-repository"]
        ),
        "true"
    );
    let shared = git(&site.repo("sandbox"), &["config", "core.sharedRepository"]);
    assert!(shared == "1" || shared == "group", "{shared}");
    let checkout = site.checkout(&project, "sandbox");
    assert!(checkout.join(".git").is_dir());
    assert_eq!(
        git(&checkout, &["remote", "get-url", "origin"]),
        format!("file://{}/sandbox.git", site.lanes.display())
    );
    let text = config_text(&project);
    assert!(text.contains("[pump.targets.sandbox]"), "{text}");
    assert!(text.contains("path = \"sandbox\""), "{text}");
    assert!(
        text.contains(&format!(
            "remote_url = \"file://{}/sandbox.git\"",
            site.lanes.display()
        )),
        "{text}"
    );
    assert_eq!(target_ids(&project), vec!["sandbox".to_string()]);
    assert_eq!(
        added.target.as_ref().unwrap().git_url,
        format!("file://{}/sandbox.git", site.lanes.display())
    );
}

#[test]
fn without_a_prefix_the_clone_uses_the_local_lane_path() {
    let site = Site::new();
    let project = site.project("app");
    let added = add_split_with(&site.request(&project, "sandbox"), &|_| Ok(())).unwrap();

    let checkout = site.checkout(&project, "sandbox");
    assert_eq!(
        git(&checkout, &["remote", "get-url", "origin"]),
        site.repo("sandbox").display().to_string()
    );
    let text = config_text(&project);
    assert!(text.contains("[pump.targets.sandbox]"), "{text}");
    assert!(!text.contains("remote_url"), "{text}");
    assert_eq!(
        added.target.as_ref().unwrap().git_url,
        site.repo("sandbox").display().to_string()
    );
}

#[test]
fn the_seed_commit_lands_on_the_lane_and_the_clone() {
    let site = Site::new();
    let project = site.project("app");
    let mut request = site.prefixed(&project, "sandbox");
    request.seed = true;
    let added = add_split_with(&request, &|_| Ok(())).unwrap();

    let commit = added.lane.as_ref().unwrap().initial_commit.clone();
    let commit = commit.expect("seed commit");
    assert_eq!(
        git(&site.repo("sandbox"), &["rev-parse", "refs/heads/main"]),
        commit
    );
    let checkout = site.checkout(&project, "sandbox");
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), commit);
}

#[cfg(unix)]
#[test]
fn a_read_only_project_root_runs_only_the_lanes_half() {
    let site = Site::new();
    let project = site.project("app");
    let mut request = site.prefixed(&project, "sandbox");
    request.seed = true;
    deny_writes(&project);
    let added = add_split_with(&request, &|_| Ok(())).unwrap();
    allow_writes(&project);

    assert!(added.target.is_none(), "{added:?}");
    assert_eq!(
        git(
            &site.repo("sandbox"),
            &["rev-parse", "--is-bare-repository"]
        ),
        "true"
    );
    assert!(added.lane.as_ref().unwrap().initial_commit.is_some());
    assert!(!site.checkout(&project, "sandbox").exists());
    assert!(target_ids(&project).is_empty());

    let remaining = added.remaining.as_ref().unwrap();
    assert_eq!(remaining.half, Half::Projects);
    assert_eq!(remaining.root, project);
    // The exact command for the user who owns the project root.
    assert_eq!(
        remaining.command,
        format!(
            "vivi project lane add {} sandbox --lanes-root {} --git-url-prefix {} --seed",
            project.display(),
            site.lanes.display(),
            site.prefix()
        )
    );
}

#[cfg(unix)]
#[test]
fn a_read_only_lanes_root_runs_only_the_projects_half() {
    let site = Site::new();
    let project = site.project("app");
    site.make_lane(&project, "sandbox", true);
    let request = site.prefixed(&project, "sandbox");
    deny_writes(&site.lanes);
    let added = add_split_with(&request, &|_| Ok(())).unwrap();
    allow_writes(&site.lanes);

    assert!(added.lane.is_none(), "{added:?}");
    let target = added.target.as_ref().unwrap();
    assert_eq!(target.target_id, "sandbox");
    assert_eq!(target.checkout_path, site.checkout(&project, "sandbox"));
    assert_eq!(
        git(
            &site.checkout(&project, "sandbox"),
            &["remote", "get-url", "origin"]
        ),
        format!("file://{}/sandbox.git", site.lanes.display())
    );
    assert_eq!(target_ids(&project), vec!["sandbox".to_string()]);

    let remaining = added.remaining.as_ref().unwrap();
    assert_eq!(remaining.half, Half::Lanes);
    assert_eq!(remaining.root, site.lanes);
    assert_eq!(
        remaining.command,
        format!(
            "vivi project lane add {} sandbox --lanes-root {} --git-url-prefix {}",
            project.display(),
            site.lanes.display(),
            site.prefix()
        )
    );
}

#[cfg(unix)]
#[test]
fn no_writable_root_is_an_error_that_names_both_roots() {
    let site = Site::new();
    let project = site.project("app");
    let request = site.prefixed(&project, "sandbox");
    deny_writes(&site.lanes);
    deny_writes(&project);
    let error = add_split_with(&request, &|_| Ok(()))
        .unwrap_err()
        .to_string();
    allow_writes(&site.lanes);
    allow_writes(&project);
    assert!(error.contains("no half can be created"), "{error}");
    assert!(
        error.contains(site.lanes.display().to_string().as_str())
            && error.contains(project.display().to_string().as_str()),
        "{error}"
    );
    assert!(!site.repo("sandbox").exists() && !site.checkout(&project, "sandbox").exists());
    assert!(target_ids(&project).is_empty());
}

#[test]
fn a_failure_at_each_step_leaves_nothing_behind() {
    for step in [Step::Lane, Step::Clone, Step::Target] {
        let site = Site::new();
        let project = site.project("app");
        let error =
            add_split_with(&site.prefixed(&project, "sandbox"), &fail_at(step)).unwrap_err();
        assert_eq!(error.to_string(), "injected failure", "{step:?}");
        assert!(!site.repo("sandbox").exists(), "{step:?}");
        assert!(!site.checkout(&project, "sandbox").exists(), "{step:?}");
        assert!(target_ids(&project).is_empty(), "{step:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_projects_half_failure_leaves_the_lanes_owners_repo_alone() {
    for step in [Step::Clone, Step::Target] {
        let site = Site::new();
        let project = site.project("app");
        site.make_lane(&project, "sandbox", true);
        let request = site.prefixed(&project, "sandbox");
        deny_writes(&site.lanes);
        let error = add_split_with(&request, &fail_at(step)).unwrap_err();
        allow_writes(&site.lanes);
        assert_eq!(error.to_string(), "injected failure", "{step:?}");
        assert!(
            site.repo("sandbox").exists(),
            "the lanes half's repository is not this run's to remove: {step:?}"
        );
        assert!(!site.checkout(&project, "sandbox").exists(), "{step:?}");
        assert!(target_ids(&project).is_empty(), "{step:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_running_pump_refuses_the_target_and_rolls_everything_back() {
    use std::fs::File;

    let site = Site::new();
    let project = site.project("app");
    // A running pump: this test holds the lock, then the store exists with
    // mode running (the order a live pump starts in).
    let home = project.join(".vivi/pump");
    fs::create_dir_all(&home).unwrap();
    let lock = File::create(home.join("lock")).unwrap();
    lock.try_lock().unwrap();
    let mut store = PumpStore::open_for_run(&home.join(DB_FILENAME)).unwrap();
    store.initialize_control("running").unwrap();
    drop(store);

    let error = add_split_with(&site.prefixed(&project, "sandbox"), &|_| Ok(()))
        .unwrap_err()
        .to_string();
    drop(lock);

    assert!(error.contains("the pump is running"), "{error}");
    assert!(!site.repo("sandbox").exists());
    assert!(!site.checkout(&project, "sandbox").exists());
    assert!(target_ids(&project).is_empty());
}

#[test]
fn a_second_add_for_the_same_target_is_refused() {
    let site = Site::new();
    let project = site.project("app");
    let request = site.prefixed(&project, "sandbox");
    add_split_with(&request, &|_| Ok(())).unwrap();

    // The lane repository already exists.
    let again = add_split_with(&request, &|_| Ok(()))
        .unwrap_err()
        .to_string();
    assert!(again.contains("already exists"), "{again}");

    // Repository gone, target entry still there: refused at the target,
    // before any clone, and this run's fresh repository is rolled back.
    fs::remove_dir_all(site.repo("sandbox")).unwrap();
    let again = add_split_with(&request, &|_| Ok(()))
        .unwrap_err()
        .to_string();
    assert!(
        again.contains("pump target `sandbox` already exists"),
        "{again}"
    );
    assert!(!site.repo("sandbox").exists());
    assert!(
        site.checkout(&project, "sandbox").join(".git").is_dir(),
        "the first run's checkout is untouched"
    );

    // Target entry gone too, checkout still there: refused at the checkout.
    targets::remove(&project, "sandbox").unwrap();
    let again = add_split_with(&request, &|_| Ok(()))
        .unwrap_err()
        .to_string();
    assert!(
        again.contains("target checkout") && again.contains("already exists"),
        "{again}"
    );
    assert!(!site.repo("sandbox").exists());
    assert_eq!(target_ids(&project), Vec::<String>::new());
}

#[test]
fn bad_ids_and_addresses_are_refused_before_anything_is_made() {
    let site = Site::new();
    let project = site.project("app");
    let mut bad_id = site.prefixed(&project, "Bad_Id");
    bad_id.target_id = "Bad_Id".into();
    let mut bad_url = site.prefixed(&project, "sandbox");
    bad_url.git_url_prefix = Some("https://example.com/".into());
    let cases: [(&str, LaneAdd, &str); 2] = [
        (
            "id",
            bad_id,
            "must use lowercase letters, digits and hyphens",
        ),
        ("url", bad_url, "http(s) address"),
    ];
    for (label, request, expected) in cases {
        let error = add_split_with(&request, &|_| Ok(()))
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{label}: {error}");
        assert!(!site.repo("sandbox").exists(), "{label}");
        assert!(!site.checkout(&project, "sandbox").exists(), "{label}");
        assert!(target_ids(&project).is_empty(), "{label}");
    }
}

#[cfg(unix)]
#[test]
fn a_missing_lane_repository_is_named_when_only_the_projects_half_runs() {
    let site = Site::new();
    let project = site.project("app");
    let request = site.prefixed(&project, "sandbox");
    deny_writes(&site.lanes);
    let error = add_split_with(&request, &|_| Ok(()))
        .unwrap_err()
        .to_string();
    allow_writes(&site.lanes);
    assert!(
        error.contains("does not exist") && error.contains("the lanes half must run first"),
        "{error}"
    );
    assert!(
        error.contains(site.repo("sandbox").display().to_string().as_str()),
        "{error}"
    );
    assert!(!site.checkout(&project, "sandbox").exists());
    assert!(target_ids(&project).is_empty());
}
