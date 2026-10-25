//! Tests for [`crate::project_create`].

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::{TempDir, tempdir};

use super::{ProjectCreate, Step, check_name, create_at, execute, lane_url};
use crate::VivariumError;
use crate::pump::config::targets;
use crate::registry;

const PREFIX: &str = "git@hub.example.ts.net:";

struct Host {
    _tmp: TempDir,
    projects: PathBuf,
    lanes: PathBuf,
    registry: PathBuf,
}

impl Host {
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

    fn request(&self, name: &str) -> ProjectCreate {
        ProjectCreate::new(name, &self.projects, &self.lanes, PREFIX)
    }

    fn project(&self, name: &str) -> PathBuf {
        self.projects.join(name)
    }

    fn repo(&self, name: &str, target: &str) -> PathBuf {
        self.lanes.join(name).join(format!("{target}.git"))
    }

    fn registered(&self) -> usize {
        registry::load_at(&self.registry).unwrap().entries().len()
    }

    fn write_base(&self, text: &str) {
        fs::create_dir_all(self.projects.join(".vivi")).unwrap();
        fs::write(self.projects.join(".vivi/mailspace.toml"), text).unwrap();
    }

    fn nothing_left(&self, name: &str) -> bool {
        !self.lanes.join(name).exists() && !self.project(name).exists() && self.registered() == 0
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
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

const BASE: &str = "base = true\n[pools.studio]\nprovider = \"local\"\nslots = 2\n";

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
fn creates_the_lane_checkout_target_mailspace_and_registration() {
    let host = Host::new();
    let created = create_at(&host.request("app"), &host.registry).unwrap();

    let project = host.project("app");
    let repo = host.repo("app", "app");
    assert_eq!(created.project_root, project);
    assert_eq!(created.lanes_dir, host.lanes.join("app"));
    assert_eq!(created.target, "app");
    assert_eq!(created.git_url, "git@hub.example.ts.net:app/app.git");
    assert_eq!(created.repo_path, repo);
    assert_eq!(created.checkout_path, project.join("app"));
    assert!(created.initial_commit.is_none());

    assert_eq!(git(&repo, &["rev-parse", "--is-bare-repository"]), "true");
    assert_eq!(git(&repo, &["config", "core.sharedRepository"]), "1");
    assert_eq!(git(&repo, &["symbolic-ref", "HEAD"]), "refs/heads/main");
    assert_eq!(
        git(&project.join("app"), &["remote", "get-url", "origin"]),
        "git@hub.example.ts.net:app/app.git"
    );

    let text = config_text(&project);
    assert!(text.contains("[pump.targets.app]"), "{text}");
    assert!(text.contains("path = \"app\""), "{text}");
    assert!(
        text.contains("remote_url = \"git@hub.example.ts.net:app/app.git\""),
        "{text}"
    );
    assert!(text.contains("main = \"main\""), "{text}");
    let ids: Vec<String> = targets::list(&project)
        .unwrap()
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(ids, ["app"]);

    let flags = registry::load_at(&host.registry)
        .unwrap()
        .get(&project)
        .unwrap();
    assert!(flags.pump && flags.serve);
    assert!(!created.readiness.ready);
}

#[test]
fn a_named_first_target_names_the_lane_and_the_checkout() {
    let host = Host::new();
    let mut request = host.request("faber.lang");
    request.target = Some("radix".into());
    let created = create_at(&request, &host.registry).unwrap();
    assert_eq!(created.repo_path, host.repo("faber.lang", "radix"));
    assert_eq!(
        created.git_url,
        "git@hub.example.ts.net:faber.lang/radix.git"
    );
    assert!(host.project("faber.lang").join("radix/.git").is_dir());
}

#[test]
fn a_name_that_is_not_a_target_id_needs_an_explicit_target() {
    let host = Host::new();
    let error = create_at(&host.request("faber.lang"), &host.registry)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("name the first target explicitly"),
        "{error}"
    );
    assert!(host.nothing_left("faber.lang"));
}

#[test]
fn flags_are_registered_as_given() {
    let host = Host::new();
    let mut request = host.request("app");
    request.pump = false;
    request.serve = false;
    create_at(&request, &host.registry).unwrap();
    let flags = registry::load_at(&host.registry)
        .unwrap()
        .get(&host.project("app"))
        .unwrap();
    assert!(!flags.pump && !flags.serve);
}

#[test]
fn inherited_pool_gets_the_first_targets_url() {
    let host = Host::new();
    host.write_base(BASE);
    let mut request = host.request("app");
    request.pool = Some("studio".into());
    create_at(&request, &host.registry).unwrap();
    let text = config_text(&host.project("app"));
    assert!(text.contains("[pools.studio]"), "{text}");
    assert!(
        text.contains("remote_url = \"git@hub.example.ts.net:app/app.git\""),
        "{text}"
    );
}

#[test]
fn a_pool_no_base_defines_fails_and_rolls_back() {
    let host = Host::new();
    let mut request = host.request("app");
    request.pool = Some("studio".into());
    let error = create_at(&request, &host.registry).unwrap_err().to_string();
    assert!(error.contains("not defined by a base config"), "{error}");
    assert!(host.nothing_left("app"));
}

#[test]
fn names_are_one_safe_segment() {
    for good in ["app", "a1", "my-app", "my.app", "my_app", "9lives"] {
        assert!(check_name(good).is_ok(), "{good}");
    }
    for bad in [
        "", "App", "-app", ".app", "_app", "a/b", "a b", "app.git", "app.", "..",
    ] {
        assert!(check_name(bad).is_err(), "{bad:?}");
    }
    assert!(check_name(&"a".repeat(65)).is_err());
}

#[test]
fn bad_inputs_are_refused_before_anything_is_made() {
    let host = Host::new();
    let mut bad_branch = host.request("app");
    bad_branch.branch = "-x".into();
    let mut http = host.request("app");
    http.git_url_prefix = "https://example.com/".into();
    let mut bad_target = host.request("app");
    bad_target.target = Some("Radix".into());
    let missing_root = ProjectCreate::new("app", host.projects.join("nope"), &host.lanes, PREFIX);
    for request in [bad_branch, http, bad_target, missing_root] {
        assert!(create_at(&request, &host.registry).is_err());
        assert!(host.nothing_left("app"));
    }
}

#[test]
fn an_existing_lanes_directory_is_refused_and_left_alone() {
    let host = Host::new();
    fs::create_dir(host.lanes.join("app")).unwrap();
    fs::write(host.lanes.join("app/keep"), "x").unwrap();
    let error = create_at(&host.request("app"), &host.registry)
        .unwrap_err()
        .to_string();
    assert!(error.contains("lanes directory"), "{error}");
    assert!(host.lanes.join("app/keep").is_file());
    assert!(!host.project("app").exists());
}

#[test]
fn an_existing_project_root_is_refused_and_left_alone() {
    let host = Host::new();
    fs::create_dir(host.project("app")).unwrap();
    let error = create_at(&host.request("app"), &host.registry)
        .unwrap_err()
        .to_string();
    assert!(error.contains("project root"), "{error}");
    assert!(host.project("app").is_dir());
    assert!(!host.lanes.join("app").exists());
}

#[test]
fn an_already_registered_root_is_refused() {
    let host = Host::new();
    let root = host.project("app");
    registry::update_at(&host.registry, |registry| {
        Ok(registry.add(root.clone(), registry::Flags::default()))
    })
    .unwrap();
    let error = create_at(&host.request("app"), &host.registry)
        .unwrap_err()
        .to_string();
    assert!(error.contains("already registered"), "{error}");
    assert!(!host.lanes.join("app").exists());
}

#[test]
fn a_failure_at_each_step_leaves_nothing_behind() {
    for step in [
        Step::Lanes,
        Step::Mailspace,
        Step::Repo,
        Step::Target,
        Step::Values,
        Step::Register,
        Step::Done,
    ] {
        let host = Host::new();
        let error = execute(&host.request("app"), &host.registry, &fail_at(step))
            .unwrap_err()
            .to_string();
        assert!(error.contains("injected failure"), "{step:?}: {error}");
        assert!(host.nothing_left("app"), "{step:?}");
    }
}

#[test]
fn initial_commit_makes_exactly_one_commit_on_the_branch() {
    let host = Host::new();
    let mut request = host.request("app");
    request.initial_commit = true;
    request.branch = "trunk".into();
    let created = create_at(&request, &host.registry).unwrap();
    let repo = host.repo("app", "app");
    let commit = created.initial_commit.unwrap();
    assert_eq!(git(&repo, &["rev-parse", "refs/heads/trunk"]), commit);
    assert_eq!(git(&repo, &["rev-list", "--count", "trunk"]), "1");
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%an <%ae>"]),
        "vivi <vivi@localhost>"
    );
    let checkout = host.project("app").join("app");
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), commit);
    assert!(config_text(&host.project("app")).contains("main = \"trunk\""));
}

#[test]
fn initial_commit_uses_the_inherited_pump_committer() {
    let host = Host::new();
    host.write_base(
        "base = true\n[pump]\ncommitter_name = \"Hub Pump\"\ncommitter_email = \"pump@hub\"\n",
    );
    let mut request = host.request("app");
    request.initial_commit = true;
    create_at(&request, &host.registry).unwrap();
    assert_eq!(
        git(
            &host.repo("app", "app"),
            &["log", "-1", "--format=%an <%ae>"]
        ),
        "Hub Pump <pump@hub>"
    );
}

#[test]
fn without_an_initial_commit_the_repo_stays_empty() {
    let host = Host::new();
    create_at(&host.request("app"), &host.registry).unwrap();
    let repo = host.repo("app", "app");
    let out = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["rev-parse", "--verify", "--quiet", "refs/heads/main"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn two_projects_with_the_same_target_id_do_not_collide() {
    let host = Host::new();
    for name in ["one", "two"] {
        let mut request = host.request(name);
        request.target = Some("core".into());
        create_at(&request, &host.registry).unwrap();
    }
    assert!(host.repo("one", "core").is_dir());
    assert!(host.repo("two", "core").is_dir());
    assert_eq!(host.registered(), 2);
}

#[test]
fn lane_urls_name_the_project_and_the_target() {
    assert_eq!(
        lane_url("git@hub:", "vivi", "vivi-server"),
        "git@hub:vivi/vivi-server.git"
    );
}

/// A plain repository with two commits on `main` and a tag, to import from.
fn source_repo(host: &Host) -> (PathBuf, String) {
    let src = host.projects.parent().unwrap().join("source");
    fs::create_dir(&src).unwrap();
    git(&src, &["init", "--quiet", "-b", "main"]);
    for (file, text) in [("a.txt", "one"), ("b.txt", "two")] {
        fs::write(src.join(file), text).unwrap();
        git(&src, &["add", file]);
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
                file,
            ],
        );
    }
    git(&src, &["tag", "v1"]);
    let tip = git(&src, &["rev-parse", "HEAD"]);
    (src, tip)
}

#[test]
fn a_source_fills_the_first_target_with_its_history() {
    let host = Host::new();
    let (src, tip) = source_repo(&host);
    let mut request = host.request("app");
    request.source = Some(src.display().to_string());
    let created = create_at(&request, &host.registry).unwrap();
    assert_eq!(created.imported.as_deref(), Some(tip.as_str()));
    let repo = host.repo("app", "app");
    assert_eq!(git(&repo, &["rev-parse", "main"]), tip);
    assert_eq!(git(&repo, &["rev-list", "--count", "main"]), "2");
    assert_eq!(git(&repo, &["tag"]), "v1");
    let checkout = host.project("app").join("app");
    assert!(checkout.join("b.txt").is_file());
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), tip);
}

#[test]
fn a_source_without_the_branch_is_refused_and_rolled_back() {
    let host = Host::new();
    let (src, _) = source_repo(&host);
    let mut request = host.request("app");
    request.source = Some(src.display().to_string());
    request.branch = "trunk".into();
    let error = create_at(&request, &host.registry).unwrap_err().to_string();
    assert!(error.contains("has no branch \"trunk\""), "{error}");
    assert!(host.nothing_left("app"));
}

#[test]
fn a_source_and_an_initial_commit_are_refused_together() {
    let host = Host::new();
    let mut request = host.request("app");
    request.source = Some("https://example.com/app.git".into());
    request.initial_commit = true;
    let error = create_at(&request, &host.registry).unwrap_err().to_string();
    assert!(error.contains("drop the initial commit"), "{error}");
    assert!(host.nothing_left("app"));
}
