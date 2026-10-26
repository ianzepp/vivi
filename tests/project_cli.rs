//! `vivi project create`, `lane add` and `delete` end to end, with the host
//! registry in a temp dir.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::{TempDir, tempdir};

/// Author and committer recorded by the fixture repositories, because the
/// isolated environment supplies no Git identity of its own.
const TEST_NAME: &str = "vivi-project-cli-test";
const TEST_EMAIL: &str = "vivi-project-cli-test@localhost";

fn vivi(config_dir: &Path, git_global: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vivi"));
    command.env("VIVI_CONFIG_DIR", config_dir).args(args);
    isolate_git_env(&mut command, git_global);
    command.output().unwrap()
}

/// A `git` command run in `cwd`, isolated from the host's Git configuration.
fn git(cwd: &Path, git_global: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.current_dir(cwd).args(args);
    isolate_git_env(&mut command, git_global);
    command
}

/// Strip every inherited `GIT_*` variable from `command`, then disable
/// system configuration and point global configuration at the empty file
/// `git_global`. The changes are per-Command: the process environment is
/// never touched, and Git children Vivi spawns inherit only this clean
/// environment, so no host setting and no inherited repository, object, or
/// configuration injection can reach a fixture.
fn isolate_git_env(command: &mut Command, git_global: &Path) {
    let inherited: Vec<OsString> = std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| key.as_encoded_bytes().starts_with(b"GIT_"))
        .collect();
    for key in inherited {
        command.env_remove(key);
    }
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command.env("GIT_CONFIG_GLOBAL", git_global);
}

/// A temporary Git repository with one empty commit on `main`, for `--source`
/// lanes. The commit carries the explicit test identity.
struct SourceRepo {
    _tmp: TempDir,
    /// `file://` address of the repository.
    url: String,
}

impl SourceRepo {
    fn new() -> Self {
        let tmp = tempdir().unwrap();
        let git_global = tmp.path().join("git-global-config");
        fs::write(&git_global, b"").unwrap();
        let repo = tmp.path().join("source");
        fs::create_dir(&repo).unwrap();
        let run = |args: &[&str]| {
            let output = git(&repo, &git_global, args).output().unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        run(&["init", "--quiet", "-b", "main"]);
        run(&["config", "user.name", TEST_NAME]);
        run(&["config", "user.email", TEST_EMAIL]);
        run(&["commit", "--allow-empty", "--quiet", "-m", "source"]);
        Self {
            _tmp: tmp,
            url: format!("file://{}", repo.canonicalize().unwrap().display()),
        }
    }
}

/// The repository-local boolean `key` of the Git repository at `repo`, or
/// `None` when it is unset there. The read runs with the isolated
/// environment, so a system, global, or injected setting can never answer
/// for the repository's own local configuration.
fn local_config_bool(host: &Host, repo: &Path, key: &str) -> Option<bool> {
    let output = git(
        repo,
        &host.git_global,
        &["config", "--local", "--bool", "--get", key],
    )
    .output()
    .unwrap();
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim() == "true")
}

fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

struct Host {
    _tmp: TempDir,
    projects: PathBuf,
    lanes: PathBuf,
    config: PathBuf,
    /// Empty file every isolated command reads as its global Git config.
    git_global: PathBuf,
}

impl Host {
    fn new() -> Self {
        let tmp = tempdir().unwrap();
        let (projects, lanes, config) = (
            tmp.path().join("projects"),
            tmp.path().join("lanes"),
            tmp.path().join("config"),
        );
        fs::create_dir(&projects).unwrap();
        fs::create_dir(&lanes).unwrap();
        let git_global = tmp.path().join("git-global-config");
        fs::write(&git_global, b"").unwrap();
        Self {
            _tmp: tmp,
            projects: projects.canonicalize().unwrap(),
            lanes: lanes.canonicalize().unwrap(),
            config,
            git_global,
        }
    }

    fn roots(&self) -> [String; 4] {
        [
            "--projects-root".into(),
            self.projects.display().to_string(),
            "--lanes-root".into(),
            self.lanes.display().to_string(),
        ]
    }

    fn run(&self, head: &[&str], tail: &[&str]) -> Output {
        let roots = self.roots();
        let mut args: Vec<&str> = head.to_vec();
        args.extend(roots.iter().map(String::as_str));
        args.extend(tail);
        vivi(&self.config, &self.git_global, &args)
    }

    fn create(&self, tail: &[&str]) -> Output {
        let mut all = vec!["--git-url-prefix", "git@hub.example.ts.net:"];
        all.extend(tail);
        self.run(&["project", "create", "app"], &all)
    }
}

#[test]
fn create_prints_the_result_and_registers_the_project() {
    let host = Host::new();
    let out = ok(&host.create(&["--initial-commit", "--no-serve", "--json"]));
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["git_url"], "git@hub.example.ts.net:app/app.git");
    assert_eq!(json["target"], "app");
    assert_eq!(json["readiness"]["ready"], false);
    assert!(json["initial_commit"].is_string());

    assert!(host.lanes.join("app/app.git/HEAD").is_file());
    assert!(host.projects.join("app/app/.git").is_dir());
    let registry = fs::read_to_string(host.config.join("vivi/pump-service.toml")).unwrap();
    assert!(
        registry.contains("pump = true") && registry.contains("serve = false"),
        "{registry}"
    );

    // The same name again is refused and changes nothing.
    let again = host.create(&[]);
    assert!(!again.status.success());
    let message = String::from_utf8_lossy(&again.stderr);
    assert!(message.contains("already exists"), "{message}");
}

#[test]
fn create_text_output_names_the_target_and_the_missing_items() {
    let host = Host::new();
    let out = ok(&host.create(&["--target", "core"]));
    assert!(out.contains("target    core"), "{out}");
    assert!(
        out.contains("git url   git@hub.example.ts.net:app/core.git"),
        "{out}"
    );
    assert!(out.contains("ready     no:"), "{out}");
}

#[test]
fn create_refuses_an_http_prefix() {
    let host = Host::new();
    let output = host.run(
        &["project", "create", "app"],
        &["--git-url-prefix", "https://example.com/"],
    );
    assert!(!output.status.success());
    assert!(!host.lanes.join("app").exists() && !host.projects.join("app").exists());
}

#[test]
fn lane_add_adds_a_lane_under_the_project() {
    let host = Host::new();
    ok(&host.create(&[]));
    let project = host.projects.join("app");
    let prefix = format!("file://{}/", host.lanes.display());
    let output = vivi(
        &host.config,
        &host.git_global,
        &[
            "project",
            "lane",
            "add",
            project.to_str().unwrap(),
            "sandbox",
            "--lanes-root",
            host.lanes.to_str().unwrap(),
            "--git-url-prefix",
            &prefix,
            "--seed",
        ],
    );
    let out = ok(&output);
    assert!(out.contains("target    sandbox"), "{out}");
    assert!(out.contains("commit    "), "{out}");
    assert!(
        out.contains(&format!("git url   {prefix}app/sandbox.git")),
        "{out}"
    );
    assert!(host.lanes.join("app/sandbox.git/HEAD").is_file());
    assert!(project.join("sandbox/.git").is_dir());
    let text = fs::read_to_string(project.join(".vivi/mailspace.toml")).unwrap();
    assert!(text.contains("[pump.targets.sandbox]"), "{text}");
}

#[test]
fn delete_needs_the_name_twice_then_removes_everything() {
    let host = Host::new();
    ok(&host.create(&[]));

    let refused = host.run(&["project", "delete", "app"], &["--confirm", "nope"]);
    assert!(!refused.status.success());
    assert!(host.projects.join("app").is_dir());

    let out = ok(&host.run(&["project", "delete", "app"], &["--confirm", "app"]));
    assert!(out.contains("deleted   app"), "{out}");
    assert!(out.contains("unregistered"), "{out}");
    assert!(!host.projects.join("app").exists());
    assert!(!host.lanes.join("app").exists());
    let registry = fs::read_to_string(host.config.join("vivi/pump-service.toml")).unwrap();
    assert!(!registry.contains("app"), "{registry}");
}
