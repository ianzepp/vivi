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
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "",
        "the library prints nothing"
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
