//! `vivi project create` end to end, with the host registry in a temp dir.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::tempdir;

fn vivi(config_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .env("VIVI_CONFIG_DIR", config_dir)
        .args(args)
        .output()
        .unwrap()
}

fn create_args<'a>(projects: &'a str, lanes: &'a str) -> Vec<&'a str> {
    vec![
        "project",
        "create",
        "app",
        "--projects-root",
        projects,
        "--lanes-root",
        lanes,
        "--git-url-prefix",
        "git@hub.example.ts.net:",
    ]
}

#[test]
fn create_prints_the_result_and_registers_the_project() {
    let tmp = tempdir().unwrap();
    let (projects, lanes, config) = (
        tmp.path().join("projects"),
        tmp.path().join("lanes"),
        tmp.path().join("config"),
    );
    fs::create_dir(&projects).unwrap();
    fs::create_dir(&lanes).unwrap();
    let (p, l) = (projects.to_str().unwrap(), lanes.to_str().unwrap());

    let mut args = create_args(p, l);
    args.extend(["--initial-commit", "--no-serve", "--json"]);
    let output = vivi(&config, &args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("lane ACL copy skipped"),
        "the ACL copy warns instead of failing the create, got {stderr}"
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["git_url"], "git@hub.example.ts.net:app.git");
    assert_eq!(json["readiness"]["ready"], false);
    assert!(json["initial_commit"].is_string());

    assert!(lanes.join("app.git/HEAD").is_file());
    let text = fs::read_to_string(projects.join("app/.vivi/mailspace.toml")).unwrap();
    assert!(
        !text.contains("branch ="),
        "no [pump] branch key is written: {text}"
    );
    let registry = fs::read_to_string(config.join("vivi/pump-service.toml")).unwrap();
    assert!(registry.contains("app"), "{registry}");
    assert!(
        registry.contains("pump = true") && registry.contains("serve = false"),
        "{registry}"
    );

    // The same name again is refused and changes nothing.
    let again = vivi(&config, &create_args(p, l));
    assert!(!again.status.success());
    let message = String::from_utf8_lossy(&again.stderr);
    assert!(message.contains("already exists"), "{message}");
}

#[test]
fn create_text_output_names_the_missing_items() {
    let tmp = tempdir().unwrap();
    let (projects, lanes, config) = (
        tmp.path().join("projects"),
        tmp.path().join("lanes"),
        tmp.path().join("config"),
    );
    fs::create_dir(&projects).unwrap();
    fs::create_dir(&lanes).unwrap();
    let output = vivi(
        &config,
        &create_args(projects.to_str().unwrap(), lanes.to_str().unwrap()),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(
        out.contains("git url   git@hub.example.ts.net:app.git"),
        "{out}"
    );
    assert!(
        out.contains("ready     no:") && out.contains("committer"),
        "{out}"
    );
}

#[test]
fn create_refuses_an_http_prefix() {
    let tmp = tempdir().unwrap();
    let (projects, lanes) = (tmp.path().join("projects"), tmp.path().join("lanes"));
    fs::create_dir(&projects).unwrap();
    fs::create_dir(&lanes).unwrap();
    let mut args = create_args(projects.to_str().unwrap(), lanes.to_str().unwrap());
    let last = args.len() - 1;
    args[last] = "https://example.com/";
    let output = vivi(&tmp.path().join("config"), &args);
    assert!(!output.status.success());
    assert!(!lanes.join("app.git").exists() && !projects.join("app").exists());
}

#[cfg(unix)]
mod split_ownership {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_read_only_lanes_root_creates_the_projects_half_and_prints_the_lanes_command() {
        let tmp = tempdir().unwrap();
        let (projects, lanes, config) = (
            tmp.path().join("projects"),
            tmp.path().join("lanes"),
            tmp.path().join("config"),
        );
        fs::create_dir(&projects).unwrap();
        fs::create_dir(&lanes).unwrap();
        let (p, l) = (projects.to_str().unwrap(), lanes.to_str().unwrap());
        fs::set_permissions(&lanes, fs::Permissions::from_mode(0o555)).unwrap();

        let output = vivi(&config, &create_args(p, l));
        fs::set_permissions(&lanes, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "",
            "the library prints nothing"
        );

        // The projects half ran; the lanes half did not.
        assert!(projects.join("app/.vivi/mailspace.toml").is_file());
        assert!(!lanes.join("app.git").exists());
        let registry = fs::read_to_string(config.join("vivi/pump-service.toml")).unwrap();
        assert!(registry.contains("app"), "{registry}");

        let out = String::from_utf8_lossy(&output.stdout);
        assert!(out.contains("created   "), "{out}");
        assert!(
            out.contains("git url   git@hub.example.ts.net:app.git"),
            "{out}"
        );
        assert!(
            out.contains(&format!(
                "remaining lanes half: no write permission on {}",
                lanes.canonicalize().unwrap().display()
            )),
            "{out}"
        );
        assert!(out.contains("run, as a user who can write it:"), "{out}");
        // The printed remainder is the exact lanes-half command: the same
        // invocation, which the lanes owner's run splits the same way.
        assert!(
            out.contains(&format!(
                "vivi project create app --projects-root {} --lanes-root {} \
                 --git-url-prefix git@hub.example.ts.net:",
                projects.canonicalize().unwrap().display(),
                lanes.canonicalize().unwrap().display()
            )),
            "the exact command\n{out}"
        );
    }
}

mod lane_add {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A created project and its canonical roots.
    struct Setup {
        _tmp: tempfile::TempDir,
        projects: std::path::PathBuf,
        lanes: std::path::PathBuf,
        config: std::path::PathBuf,
    }

    impl Setup {
        fn new() -> Self {
            let tmp = tempdir().unwrap();
            let (projects, lanes, config) = (
                tmp.path().join("projects"),
                tmp.path().join("lanes"),
                tmp.path().join("config"),
            );
            fs::create_dir(&projects).unwrap();
            fs::create_dir(&lanes).unwrap();
            let output = vivi(
                &config,
                &create_args(projects.to_str().unwrap(), lanes.to_str().unwrap()),
            );
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Self {
                _tmp: tmp,
                projects: projects.canonicalize().unwrap(),
                lanes: lanes.canonicalize().unwrap(),
                config,
            }
        }

        fn project(&self) -> std::path::PathBuf {
            self.projects.join("app")
        }

        fn prefix(&self) -> String {
            format!("file://{}/", self.lanes.display())
        }

        fn add_args(&self, extra: &[&str]) -> Vec<String> {
            let mut args: Vec<String> = [
                "project",
                "lane",
                "add",
                self.project().to_str().unwrap(),
                "sandbox",
                "--lanes-root",
                self.lanes.to_str().unwrap(),
                "--git-url-prefix",
                &self.prefix(),
            ]
            .iter()
            .map(|arg| arg.to_string())
            .collect();
            args.extend(extra.iter().map(|arg| arg.to_string()));
            args
        }
    }

    fn run(config: &Path, args: &[String]) -> Output {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        vivi(config, &refs)
    }

    #[test]
    fn lane_add_attaches_the_lane_and_prints_it() {
        let setup = Setup::new();
        let output = run(&setup.config, &setup.add_args(&["--seed"]));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let out = String::from_utf8_lossy(&output.stdout);
        assert!(out.contains("repo      "), "{out}");
        assert!(out.contains("commit    "), "{out}");
        assert!(out.contains("target    sandbox"), "{out}");
        assert!(
            out.contains(&format!(
                "checkout  {}",
                setup.project().join("sandbox").display()
            )),
            "{out}"
        );
        assert!(
            out.contains(&format!(
                "git url   file://{}/sandbox.git",
                setup.lanes.display()
            )),
            "{out}"
        );
        assert!(!out.contains("remaining"), "{out}");

        assert!(setup.lanes.join("sandbox.git/HEAD").is_file());
        assert!(setup.project().join("sandbox/.git").is_dir());
        let text = fs::read_to_string(setup.project().join(".vivi/mailspace.toml")).unwrap();
        assert!(text.contains("[pump.targets.sandbox]"), "{text}");
        assert!(text.contains("path = \"sandbox\""), "{text}");
    }

    #[test]
    fn lane_add_prints_json() {
        let setup = Setup::new();
        let output = run(&setup.config, &setup.add_args(&["--json"]));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json["target"]["target_id"], "sandbox");
        assert!(json["lane"]["repo_path"].is_string());
        assert!(json["remaining"].is_null());
    }

    #[cfg(unix)]
    #[test]
    fn the_printed_command_performs_the_other_half() {
        let setup = Setup::new();

        // Half 1: only the lanes root is writable.
        let project = setup.project();
        fs::set_permissions(&project, fs::Permissions::from_mode(0o555)).unwrap();
        let first = run(&setup.config, &setup.add_args(&[]));
        fs::set_permissions(&project, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            first.status.success(),
            "{}",
            String::from_utf8_lossy(&first.stderr)
        );
        let out = String::from_utf8_lossy(&first.stdout);
        assert!(
            out.contains(&format!(
                "remaining projects half: no write permission on {}",
                project.display()
            )),
            "{out}"
        );
        assert!(setup.lanes.join("sandbox.git/HEAD").is_file());
        assert!(!project.join("sandbox").exists());

        // The printed remainder is the exact command: run it as the user
        // who owns the lanes root only, and it finishes the job.
        let command = out
            .lines()
            .map(str::trim_start)
            .find(|line| line.starts_with("vivi project lane add"))
            .expect("the printed command");
        let tokens: Vec<String> = command.split_whitespace().map(String::from).collect();
        fs::set_permissions(&setup.lanes, fs::Permissions::from_mode(0o555)).unwrap();
        let second = run(&setup.config, &tokens[1..]);
        fs::set_permissions(&setup.lanes, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            second.status.success(),
            "{}\ncommand was: {command}",
            String::from_utf8_lossy(&second.stderr)
        );
        let out = String::from_utf8_lossy(&second.stdout);
        assert!(out.contains("target    sandbox"), "{out}");
        // The half this caller could not write is still reported, exactly
        // as the done_when pins: clone and target added, lanes-half command
        // printed for the user who owns that root (and already ran it).
        assert!(
            out.contains(&format!(
                "remaining lanes half: no write permission on {}",
                setup.lanes.display()
            )),
            "{out}"
        );
        assert!(project.join("sandbox/.git").is_dir());
        let text = fs::read_to_string(project.join(".vivi/mailspace.toml")).unwrap();
        assert!(text.contains("[pump.targets.sandbox]"), "{text}");
    }
}
