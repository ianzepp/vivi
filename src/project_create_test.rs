//! Tests for [`crate::project_create`].

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::{TempDir, tempdir};

use super::{
    Half, LaneCreate, ProjectCreate, Roots, Step, check_name, create_at, create_lane,
    create_project_half, create_split_at, execute, shell_arg, split_execute,
};
use crate::VivariumError;
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

    fn repo(&self, name: &str) -> PathBuf {
        self.lanes.join(format!("{name}.git"))
    }

    fn registered(&self) -> usize {
        registry::load_at(&self.registry).unwrap().entries().len()
    }

    fn write_base(&self, text: &str) {
        fs::create_dir_all(self.projects.join(".vivi")).unwrap();
        fs::write(self.projects.join(".vivi/mailspace.toml"), text).unwrap();
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
fn creates_repo_mailspace_values_and_registration() {
    let host = Host::new();
    let mut request = host.request("app");
    request.branch = "trunk".into();
    let created = create_at(&request, &host.registry).unwrap();

    assert_eq!(created.project_root, host.project("app"));
    assert_eq!(created.repo_path, host.repo("app"));
    assert_eq!(created.git_url, "git@hub.example.ts.net:app.git");
    assert_eq!(created.initial_commit, None);

    let repo = host.repo("app");
    assert_eq!(git(&repo, &["rev-parse", "--is-bare-repository"]), "true");
    // Git stores `--shared=group` as 1.
    let shared = git(&repo, &["config", "core.sharedRepository"]);
    assert!(shared == "1" || shared == "group", "{shared}");
    assert_eq!(git(&repo, &["symbolic-ref", "HEAD"]), "refs/heads/trunk");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&repo).unwrap().permissions().mode();
        assert_eq!(mode & 0o070, 0o070, "group rwx on the repo dir: {mode:o}");
        assert_ne!(mode & 0o2000, 0, "setgid on the repo dir: {mode:o}");
    }

    let text = config_text(&host.project("app"));
    assert!(
        !text.contains("branch ="),
        "no [pump] branch key is written: {text}"
    );
    assert!(!created.mailspace_id.is_empty());
    assert!(text.contains(&created.mailspace_id), "{text}");

    let entry = registry::load_at(&host.registry)
        .unwrap()
        .get(&host.project("app"));
    assert_eq!(
        entry,
        Some(registry::Flags {
            pump: true,
            serve: true
        })
    );
    assert!(!created.readiness.ready);
    assert!(
        created
            .readiness
            .missing
            .iter()
            .any(|m| m.item.contains("committer")),
        "{:?}",
        created.readiness
    );
}

#[test]
fn flags_are_registered_as_given() {
    let host = Host::new();
    let mut request = host.request("app");
    request.pump = false;
    request.serve = false;
    create_at(&request, &host.registry).unwrap();
    let entry = registry::load_at(&host.registry)
        .unwrap()
        .get(&host.project("app"));
    assert_eq!(
        entry,
        Some(registry::Flags {
            pump: false,
            serve: false
        })
    );
}

#[test]
fn inherited_pool_gets_the_local_remote_url() {
    let host = Host::new();
    host.write_base(BASE);
    let mut request = host.request("app");
    request.pool = Some("studio".into());
    create_at(&request, &host.registry).unwrap();

    let text = config_text(&host.project("app"));
    assert!(text.contains("[pools.studio]"), "{text}");
    assert!(
        text.contains("remote_url = \"git@hub.example.ts.net:app.git\""),
        "{text}"
    );
    assert!(
        !text.contains("slots"),
        "the base keeps the pool's other keys: {text}"
    );

    let mailspace = crate::mailspace::Mailspace::open_root(host.project("app")).unwrap();
    let pool = &mailspace.config.pools["studio"];
    assert_eq!(pool.slots, 2, "inherited from the base");
    assert_eq!(
        pool.remote_url.as_deref(),
        Some("git@hub.example.ts.net:app.git")
    );
}

#[test]
fn a_pool_no_base_defines_fails_and_rolls_back() {
    let host = Host::new();
    let mut request = host.request("app");
    request.pool = Some("studio".into());
    let error = create_at(&request, &host.registry).unwrap_err().to_string();
    assert!(error.contains("not defined by a base"), "{error}");
    assert!(!host.project("app").exists());
    assert!(!host.repo("app").exists());
    assert_eq!(host.registered(), 0);
}

#[test]
fn names_are_one_safe_segment() {
    for good in ["app", "a1", "my-app_v2.x", "9lives"] {
        check_name(good).unwrap();
    }
    let long = "a".repeat(65);
    for bad in [
        "", ".hidden", "-flag", "_x", "Upper", "a/b", "a b", "..", ".", "x.git", "x.", "é", &long,
    ] {
        assert!(check_name(bad).is_err(), "{bad:?} should be refused");
    }
}

#[test]
fn bad_inputs_are_refused_before_anything_is_made() {
    let host = Host::new();
    let mut cases = Vec::new();
    let mut request = host.request("app");
    request.git_url_prefix = "https://example.com/".into();
    cases.push(("http", request));
    let mut request = host.request("app");
    request.git_url_prefix = "-oProxy=x ".into();
    cases.push(("dash", request));
    let mut request = host.request("app");
    request.branch = "bad..branch".into();
    cases.push(("branch", request));
    let mut request = host.request("app");
    request.pool = Some("Bad Pool".into());
    cases.push(("pool", request));
    let mut request = host.request("app");
    request.projects_root = host.projects.join("missing");
    cases.push(("projects root", request));
    cases.push(("name", host.request("Bad/Name")));
    for (label, request) in cases {
        assert!(create_at(&request, &host.registry).is_err(), "{label}");
    }
    assert!(!host.project("app").exists() && !host.repo("app").exists());
    assert_eq!(host.registered(), 0);
}

#[test]
fn an_existing_repo_is_refused_and_left_alone() {
    let host = Host::new();
    fs::create_dir(host.repo("app")).unwrap();
    fs::write(host.repo("app").join("keep"), "mine").unwrap();
    let error = create_at(&host.request("app"), &host.registry)
        .unwrap_err()
        .to_string();
    assert!(error.contains("already exists"), "{error}");
    assert_eq!(
        fs::read_to_string(host.repo("app").join("keep")).unwrap(),
        "mine"
    );
    assert!(!host.project("app").exists());
    assert_eq!(host.registered(), 0);
}

#[test]
fn an_existing_project_root_is_refused_and_left_alone() {
    let host = Host::new();
    fs::create_dir(host.project("app")).unwrap();
    fs::write(host.project("app").join("keep"), "mine").unwrap();
    let error = create_at(&host.request("app"), &host.registry)
        .unwrap_err()
        .to_string();
    assert!(error.contains("already exists"), "{error}");
    assert_eq!(
        fs::read_to_string(host.project("app").join("keep")).unwrap(),
        "mine"
    );
    assert!(!host.repo("app").exists());
}

#[test]
fn an_already_registered_root_is_refused() {
    let host = Host::new();
    registry::update_at(&host.registry, |registry| {
        Ok(registry.add(host.project("app"), registry::Flags::default()))
    })
    .unwrap();
    let error = create_at(&host.request("app"), &host.registry)
        .unwrap_err()
        .to_string();
    assert!(error.contains("already registered"), "{error}");
    assert!(!host.repo("app").exists() && !host.project("app").exists());
    assert_eq!(host.registered(), 1);
}

#[test]
fn a_failure_at_each_step_leaves_nothing_behind() {
    for step in [
        Step::Repo,
        Step::Mailspace,
        Step::Values,
        Step::Commit,
        Step::Register,
        Step::Done,
    ] {
        let host = Host::new();
        let mut request = host.request("app");
        request.initial_commit = true;
        let error = execute(&request, &host.registry, &fail_at(step)).unwrap_err();
        assert_eq!(error.to_string(), "injected failure", "{step:?}");
        assert!(!host.project("app").exists(), "{step:?}");
        assert!(!host.repo("app").exists(), "{step:?}");
        assert_eq!(host.registered(), 0, "{step:?}");
        // The names are free again.
        create_at(&request, &host.registry).unwrap();
    }
}

#[test]
fn a_registration_that_lost_a_race_is_not_unregistered() {
    // Another writer registers the same root between the checks and ours:
    // the create fails, undoes its own paths, and leaves their entry alone.
    let host = Host::new();
    let request = host.request("app");
    let racing = |step: Step| {
        if step == Step::Register {
            registry::update_at(&host.registry, |registry| {
                registry.add(host.project("app"), registry::Flags::default());
                Ok(())
            })?;
        }
        Ok(())
    };
    let error = execute(&request, &host.registry, &racing)
        .unwrap_err()
        .to_string();
    assert!(error.contains("already registered"), "{error}");
    assert!(!host.project("app").exists() && !host.repo("app").exists());
    // The entry the other writer made is not ours to remove.
    assert_eq!(host.registered(), 1);
}

#[test]
fn a_corrupt_registry_fails_and_rolls_back() {
    let host = Host::new();
    fs::create_dir_all(host.registry.parent().unwrap()).unwrap();
    fs::write(&host.registry, "not = [valid").unwrap();
    assert!(create_at(&host.request("app"), &host.registry).is_err());
    assert!(!host.project("app").exists() && !host.repo("app").exists());
}

#[test]
fn a_rollback_failure_is_reported_with_the_original_error() {
    let host = Host::new();
    let request = host.request("app");
    let sabotage = |step: Step| {
        if step == Step::Values {
            // Replace the project dir by a file so removal by remove_dir_all fails.
            fs::remove_dir_all(host.project("app")).unwrap();
            fs::write(host.project("app"), "x").unwrap();
            fs::remove_dir_all(host.repo("app")).unwrap();
            return Err(VivariumError::Other("injected failure".into()));
        }
        Ok(())
    };
    let error = execute(&request, &host.registry, &sabotage)
        .unwrap_err()
        .to_string();
    assert!(error.contains("injected failure"), "{error}");
    assert!(error.contains("rollback also failed"), "{error}");
}

#[test]
fn initial_commit_makes_exactly_one_commit_on_the_branch() {
    let host = Host::new();
    let mut request = host.request("app");
    request.initial_commit = true;
    request.branch = "trunk".into();
    let created = create_at(&request, &host.registry).unwrap();
    let repo = host.repo("app");
    assert_eq!(
        git(&repo, &["rev-list", "--count", "refs/heads/trunk"]),
        "1"
    );
    assert_eq!(
        git(&repo, &["rev-parse", "refs/heads/trunk"]),
        created.initial_commit.clone().unwrap()
    );
    assert_eq!(
        git(
            &repo,
            &["log", "-1", "--format=%an <%ae>|%cn <%ce>", "trunk"]
        ),
        "vivi <vivi@localhost>|vivi <vivi@localhost>"
    );
    assert_eq!(git(&repo, &["rev-list", "--count", "--all"]), "1");
}

#[test]
fn initial_commit_uses_the_inherited_pump_committer() {
    let host = Host::new();
    host.write_base(
        "base = true\n[pump]\ncommitter_name = \"Pump\"\ncommitter_email = \"pump@example.com\"\n",
    );
    let mut request = host.request("app");
    request.initial_commit = true;
    create_at(&request, &host.registry).unwrap();
    let who = git(
        &host.repo("app"),
        &["log", "-1", "--format=%an <%ae>", "main"],
    );
    assert_eq!(who, "Pump <pump@example.com>");
}

#[test]
fn without_an_initial_commit_the_repo_stays_empty() {
    let host = Host::new();
    create_at(&host.request("app"), &host.registry).unwrap();
    let out = Command::new("git")
        .arg("-C")
        .arg(host.repo("app"))
        .args(["rev-parse", "--verify", "refs/heads/main"])
        .output()
        .unwrap();
    assert!(!out.status.success());
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
fn split_runs_both_halves_when_both_roots_are_writable() {
    let host = Host::new();
    let mut request = host.request("app");
    request.initial_commit = true;
    let split = create_split_at(&request, &host.registry).unwrap();

    assert!(split.remaining.is_none(), "{split:?}");
    let created = split.full().unwrap();
    assert_eq!(created.project_root, host.project("app"));
    assert_eq!(created.repo_path, host.repo("app"));
    assert!(created.initial_commit.is_some());
    assert_eq!(
        git(&host.repo("app"), &["rev-list", "--count", "--all"]),
        "1"
    );
    assert!(host.project("app").join(".vivi/mailspace.toml").is_file());
    assert_eq!(host.registered(), 1);
}

#[cfg(unix)]
#[test]
fn an_unwritable_lanes_root_runs_only_the_projects_half() {
    let host = Host::new();
    let mut request = host.request("app");
    request.branch = "trunk".into();
    request.initial_commit = true;
    deny_writes(&host.lanes);
    let split = create_split_at(&request, &host.registry).unwrap();
    allow_writes(&host.lanes);

    assert!(split.lane.is_none(), "{split:?}");
    let project = split.project.as_ref().unwrap();
    assert_eq!(project.project_root, host.project("app"));
    assert!(host.project("app").join(".vivi/mailspace.toml").is_file());
    assert!(!host.repo("app").exists());
    assert_eq!(host.registered(), 1);

    let remaining = split.remaining.as_ref().unwrap();
    assert_eq!(remaining.half, Half::Lanes);
    assert_eq!(remaining.root, host.lanes);
    // The exact command for the user who owns the lanes root.
    assert_eq!(
        remaining.command,
        format!(
            "vivi project create app --projects-root {} --lanes-root {} \
             --git-url-prefix {PREFIX} --branch trunk --initial-commit",
            host.projects.display(),
            host.lanes.display()
        )
    );
}

#[cfg(unix)]
#[test]
fn an_unwritable_projects_root_runs_only_the_lanes_half() {
    let host = Host::new();
    host.write_base(
        "base = true\n[pump]\ncommitter_name = \"Pump\"\ncommitter_email = \"pump@example.com\"\n",
    );
    let mut request = host.request("app");
    request.initial_commit = true;
    deny_writes(&host.projects);
    let split = create_split_at(&request, &host.registry).unwrap();
    allow_writes(&host.projects);

    assert!(split.project.is_none(), "{split:?}");
    let lane = split.lane.as_ref().unwrap();
    assert_eq!(lane.repo_path, host.repo("app"));
    assert!(lane.initial_commit.is_some());
    assert_eq!(
        git(&host.repo("app"), &["rev-parse", "--is-bare-repository"]),
        "true"
    );
    // The seed commit's identity comes from the base above the projects
    // root, read-only here, exactly as a both-halves run would resolve it.
    assert_eq!(
        git(
            &host.repo("app"),
            &["log", "-1", "--format=%an <%ae>", "main"]
        ),
        "Pump <pump@example.com>"
    );
    assert!(!host.project("app").exists());
    assert_eq!(host.registered(), 0);

    let remaining = split.remaining.as_ref().unwrap();
    assert_eq!(remaining.half, Half::Projects);
    assert_eq!(remaining.root, host.projects);
    assert_eq!(
        remaining.command,
        format!(
            "vivi project create app --projects-root {} --lanes-root {} \
             --git-url-prefix {PREFIX} --initial-commit",
            host.projects.display(),
            host.lanes.display()
        )
    );
}

#[test]
fn no_writable_root_is_an_error_that_names_both_roots() {
    let host = Host::new();
    let dirs = Roots::of(&host.request("app")).unwrap();
    let error = split_execute(
        &host.request("app"),
        &host.registry,
        &dirs,
        &|_| Ok(()),
        false,
        false,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("no half can be created"), "{error}");
    assert!(
        error.contains(host.lanes.display().to_string().as_str())
            && error.contains(host.projects.display().to_string().as_str()),
        "{error}"
    );
    assert!(!host.repo("app").exists() && !host.project("app").exists());
    assert_eq!(host.registered(), 0);
}

#[test]
fn a_lanes_half_failure_rolls_back_only_the_repository() {
    for step in [Step::Repo, Step::Commit] {
        let host = Host::new();
        let mut request = host.request("app");
        request.initial_commit = true;
        let dirs = Roots::of(&request).unwrap();
        let error = split_execute(&request, &host.registry, &dirs, &fail_at(step), true, false)
            .unwrap_err();
        assert_eq!(error.to_string(), "injected failure", "{step:?}");
        assert!(!host.repo("app").exists(), "{step:?}");
        assert!(!host.project("app").exists(), "{step:?}");
        assert_eq!(host.registered(), 0, "{step:?}");
    }
}

#[test]
fn a_projects_half_failure_leaves_a_repo_it_did_not_create_alone() {
    for step in [Step::Mailspace, Step::Values, Step::Register, Step::Done] {
        let host = Host::new();
        // The lanes owner already made the repository.
        create_lane(&LaneCreate::new("app", &host.lanes, &host.project("app"))).unwrap();
        let request = host.request("app");
        let dirs = Roots::of(&request).unwrap();
        let error = split_execute(&request, &host.registry, &dirs, &fail_at(step), false, true)
            .unwrap_err();
        assert_eq!(error.to_string(), "injected failure", "{step:?}");
        assert!(!host.project("app").exists(), "{step:?}");
        assert_eq!(host.registered(), 0, "{step:?}");
        assert!(
            host.repo("app").exists(),
            "the lanes half's repository is not this run's to remove: {step:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_values_failure_in_split_mode_leaves_the_lanes_owners_repo() {
    let host = Host::new();
    create_lane(&LaneCreate::new("app", &host.lanes, &host.project("app"))).unwrap();
    let mut request = host.request("app");
    request.pool = Some("studio".into());
    deny_writes(&host.lanes);
    let error = create_split_at(&request, &host.registry)
        .unwrap_err()
        .to_string();
    allow_writes(&host.lanes);
    assert!(error.contains("not defined by a base"), "{error}");
    assert!(!host.project("app").exists());
    assert_eq!(host.registered(), 0);
    assert!(host.repo("app").exists());
}

#[test]
fn create_lane_makes_a_bare_group_shared_repository() {
    let host = Host::new();
    host.write_base(
        "base = true\n[pump]\ncommitter_name = \"Pump\"\ncommitter_email = \"pump@example.com\"\n",
    );
    let mut request = LaneCreate::new("app", &host.lanes, &host.project("app"));
    request.branch = "trunk".into();
    request.initial_commit = true;
    let created = create_lane(&request).unwrap();

    assert_eq!(created.repo_path, host.repo("app"));
    assert!(created.initial_commit.is_some());
    let repo = host.repo("app");
    assert_eq!(git(&repo, &["rev-parse", "--is-bare-repository"]), "true");
    let shared = git(&repo, &["config", "core.sharedRepository"]);
    assert!(shared == "1" || shared == "group", "{shared}");
    assert_eq!(git(&repo, &["symbolic-ref", "HEAD"]), "refs/heads/trunk");
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%an <%ae>", "trunk"]),
        "Pump <pump@example.com>"
    );

    let again = create_lane(&request).unwrap_err().to_string();
    assert!(again.contains("already exists"), "{again}");
    // Nothing outside the lanes root is touched.
    assert!(!host.project("app").exists());
    assert_eq!(host.registered(), 0);
}

#[test]
fn create_project_half_attaches_to_an_existing_repository() {
    let host = Host::new();
    host.write_base(BASE);
    create_lane(&LaneCreate::new("app", &host.lanes, &host.project("app"))).unwrap();
    let mut request = host.request("app");
    request.pool = Some("studio".into());
    let created = create_project_half(&request, &host.registry).unwrap();

    assert_eq!(created.project_root, host.project("app"));
    assert_eq!(
        created.git_url, "git@hub.example.ts.net:app.git",
        "{:?}",
        created.git_url
    );
    let text = config_text(&host.project("app"));
    assert!(
        text.contains("remote_url = \"git@hub.example.ts.net:app.git\""),
        "{text}"
    );
    assert_eq!(host.registered(), 1);
    assert!(
        host.repo("app").exists(),
        "the repo is not this half's to touch"
    );
}

#[test]
fn unsafe_words_in_the_remainder_command_are_quoted() {
    assert_eq!(shell_arg("app-1.x"), "app-1.x");
    assert_eq!(shell_arg("two words"), "'two words'");
    assert_eq!(shell_arg("it's"), "'it'\\''s'");
    assert_eq!(shell_arg(""), "''");
}
