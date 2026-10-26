//! Tests for [`super`] (`project_create::lane_add`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::{TempDir, tempdir};

use super::{LaneAdd, Step, add_with};
use crate::VivariumError;
use crate::project_create::{ProjectCreate, create_at};
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
        let request = ProjectCreate::new(name, &self.projects, &self.lanes, self.prefix());
        create_at(&request, &self.registry).unwrap();
        self.projects.join(name)
    }

    /// A request whose lane URL is a real, local `file://` address.
    fn request(&self, project: &Path, id: &str) -> LaneAdd {
        let mut request = LaneAdd::new(project, id, &self.lanes);
        request.git_url_prefix = Some(self.prefix());
        request
    }

    fn prefix(&self) -> String {
        format!("file://{}/", self.lanes.display())
    }

    fn repo(&self, project: &str, id: &str) -> PathBuf {
        self.lanes.join(project).join(format!("{id}.git"))
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

#[test]
fn adds_the_repo_the_checkout_and_the_target_under_the_project() {
    let site = Site::new();
    let project = site.project("app");
    let added = add_with(&site.request(&project, "sandbox"), &|_| Ok(())).unwrap();

    let repo = site.repo("app", "sandbox");
    let url = format!("file://{}/app/sandbox.git", site.lanes.display());
    assert_eq!(added.target_id, "sandbox");
    assert_eq!(added.repo_path, repo);
    assert_eq!(added.checkout_path, project.join("sandbox"));
    assert_eq!(added.git_url, url);
    assert_eq!(git(&repo, &["rev-parse", "--is-bare-repository"]), "true");
    assert_eq!(
        git(&project.join("sandbox"), &["remote", "get-url", "origin"]),
        url
    );
    assert_eq!(target_ids(&project), ["app", "sandbox"]);
    let text = fs::read_to_string(project.join(".vivi/mailspace.toml")).unwrap();
    assert!(text.contains(&format!("remote_url = \"{url}\"")), "{text}");
    let section = text
        .split("[pump.targets.sandbox]")
        .nth(1)
        .and_then(|rest| rest.split("\n[").next())
        .unwrap_or_default();
    assert!(section.contains("main = \"vivi/main\""), "{text}");
}

#[test]
fn without_a_prefix_the_origin_is_the_local_lane_path() {
    let site = Site::new();
    let project = site.project("app");
    let mut request = site.request(&project, "sandbox");
    request.git_url_prefix = None;
    let added = add_with(&request, &|_| Ok(())).unwrap();
    let repo = site.repo("app", "sandbox");
    assert_eq!(added.git_url, repo.display().to_string());
    let text = fs::read_to_string(project.join(".vivi/mailspace.toml")).unwrap();
    let sandbox = text.split("[pump.targets.sandbox]").nth(1).unwrap();
    assert!(!sandbox.contains("remote_url"), "{text}");
}

#[test]
fn the_seed_commit_lands_on_the_lane_and_the_clone() {
    let site = Site::new();
    let project = site.project("app");
    let mut request = site.request(&project, "sandbox");
    request.seed = true;
    let added = add_with(&request, &|_| Ok(())).unwrap();
    let commit = added.initial_commit.unwrap();
    assert_eq!(
        git(&site.repo("app", "sandbox"), &["rev-parse", "main"]),
        commit
    );
    assert_eq!(
        git(&project.join("sandbox"), &["rev-parse", "HEAD"]),
        commit
    );
    // The target's main exists from the start, at the seed commit.
    assert_eq!(
        git(&site.repo("app", "sandbox"), &["rev-parse", "vivi/main"]),
        commit
    );
    assert_eq!(
        git(&project.join("sandbox"), &["rev-parse", "origin/vivi/main"]),
        commit
    );
}

#[test]
fn an_empty_lane_has_no_integration_branch_until_the_pump_makes_it() {
    let site = Site::new();
    let project = site.project("app");
    add_with(&site.request(&project, "sandbox"), &|_| Ok(())).unwrap();
    assert_eq!(
        git(
            &site.repo("app", "sandbox"),
            &["branch", "--list", "vivi/main"]
        ),
        ""
    );
}

#[test]
fn a_missing_lanes_directory_is_made_for_an_older_project() {
    let site = Site::new();
    let project = site.project("app");
    fs::remove_dir_all(site.lanes.join("app")).unwrap();
    add_with(&site.request(&project, "sandbox"), &|_| Ok(())).unwrap();
    assert!(site.repo("app", "sandbox").is_dir());
}

#[test]
fn a_failure_at_each_step_leaves_nothing_new_behind() {
    for step in [Step::Lane, Step::Target] {
        let site = Site::new();
        let project = site.project("app");
        let error = add_with(&site.request(&project, "sandbox"), &fail_at(step))
            .unwrap_err()
            .to_string();
        assert!(error.contains("injected failure"), "{step:?}: {error}");
        assert!(!site.repo("app", "sandbox").exists(), "{step:?}");
        assert!(!project.join("sandbox").exists(), "{step:?}");
        assert_eq!(target_ids(&project), ["app"], "{step:?}");
        assert!(site.repo("app", "app").is_dir(), "{step:?}");
    }
}

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

    let error = add_with(&site.request(&project, "sandbox"), &|_| Ok(()))
        .unwrap_err()
        .to_string();
    drop(lock);

    assert!(error.contains("the pump is running"), "{error}");
    assert!(!site.repo("app", "sandbox").exists());
    assert!(!project.join("sandbox").exists());
    assert_eq!(target_ids(&project), ["app"]);
}

#[test]
fn a_second_add_for_the_same_target_is_refused() {
    let site = Site::new();
    let project = site.project("app");
    add_with(&site.request(&project, "sandbox"), &|_| Ok(())).unwrap();
    let error = add_with(&site.request(&project, "sandbox"), &|_| Ok(()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("already exists"), "{error}");
    assert!(site.repo("app", "sandbox").is_dir());
    assert!(project.join("sandbox/.git").is_dir());
}

#[test]
fn the_first_target_cannot_be_added_again() {
    let site = Site::new();
    let project = site.project("app");
    let error = add_with(&site.request(&project, "app"), &|_| Ok(()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("already exists"), "{error}");
}

#[test]
fn bad_ids_and_addresses_are_refused_before_anything_is_made() {
    let site = Site::new();
    let project = site.project("app");
    let bad_id = site.request(&project, "Sand_box");
    let mut http = site.request(&project, "sandbox");
    http.git_url_prefix = Some("https://example.com/".into());
    for request in [bad_id, http] {
        assert!(add_with(&request, &|_| Ok(())).is_err());
        assert!(!site.repo("app", "sandbox").exists());
        assert_eq!(target_ids(&project), ["app"]);
    }
}

#[test]
fn a_source_fills_the_new_lane_with_its_history() {
    let site = Site::new();
    let project = site.project("app");
    let src = site.projects.parent().unwrap().join("source");
    fs::create_dir(&src).unwrap();
    git(&src, &["init", "--quiet", "-b", "main"]);
    fs::write(src.join("x.txt"), "x").unwrap();
    git(&src, &["add", "x.txt"]);
    git(
        &src,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "-m",
            "x",
        ],
    );
    let tip = git(&src, &["rev-parse", "HEAD"]);

    let mut request = site.request(&project, "server");
    request.source = Some(src.display().to_string());
    let added = add_with(&request, &|_| Ok(())).unwrap();
    assert_eq!(added.imported.as_deref(), Some(tip.as_str()));
    assert_eq!(
        git(&site.repo("app", "server"), &["rev-parse", "main"]),
        tip
    );
    assert!(project.join("server/x.txt").is_file());
    assert_eq!(
        git(&site.repo("app", "server"), &["rev-parse", "vivi/main"]),
        tip
    );
}

#[test]
fn a_vivi_main_the_source_brought_is_left_alone() {
    let site = Site::new();
    let project = site.project("app");
    let src = site.projects.parent().unwrap().join("source");
    fs::create_dir(&src).unwrap();
    git(&src, &["init", "--quiet", "-b", "main"]);
    let commit = |name: &str| {
        fs::write(src.join(name), name).unwrap();
        git(&src, &["add", name]);
        git(
            &src,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--quiet",
                "-m",
                name,
            ],
        );
        git(&src, &["rev-parse", "HEAD"])
    };
    let first = commit("one");
    git(&src, &["branch", "vivi/main"]);
    let second = commit("two");
    assert_ne!(first, second);

    let mut request = site.request(&project, "server");
    request.source = Some(src.display().to_string());
    add_with(&request, &|_| Ok(())).unwrap();
    let repo = site.repo("app", "server");
    assert_eq!(git(&repo, &["rev-parse", "main"]), second);
    assert_eq!(git(&repo, &["rev-parse", "vivi/main"]), first);
}
