//! End-to-end tests for the top-level read-only doctor command.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn vivi(project: &Path, args: &[&str]) -> Output {
    let home = project.to_string_lossy();
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .env("HOME", home.as_ref())
        .env("XDG_CONFIG_HOME", project.join("config"))
        .env("APPDATA", project.join("config"))
        .env("VIVI_PROXY_CONFIG", project.join("no-proxy.toml"))
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("https_proxy", "http://127.0.0.1:1")
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("http_proxy", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("all_proxy", "http://127.0.0.1:1")
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .env_remove("RUST_LOG")
        .args(args)
        .output()
        .expect("run vivi")
}

fn init(project: &Path) {
    let root = project.to_str().expect("UTF-8 temp path");
    let output = vivi(project, &["mailspace", "init", "--project", root]);
    assert!(output.status.success(), "{}", text(&output));
    for identity in ["mind", "cto"] {
        let output = vivi(
            project,
            &["mailspace", "identity", "add", identity, "--project", root],
        );
        assert!(output.status.success(), "{}", text(&output));
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

const FLOW_ROLES: [&str; 5] = ["researcher", "admitter", "slicer", "repairer", "head-cto"];

/// Give the flow roles what a ready project has: a local pool that serves a
/// band with a model, and each flow role assigned to that pool and band.
fn provision_flow_role_pools(project: &Path) {
    let root = project.to_str().expect("UTF-8 temp path");
    let slots = project.join("slots");
    std::fs::create_dir_all(&slots).unwrap();
    let config = project.join(".vivi/mailspace.toml");
    let mut config_text = std::fs::read_to_string(&config).unwrap();
    config_text.push_str(&format!(
        "\n[pools.main]\nprovider = \"local\"\nslots = 1\n\
         [pools.main.local]\nroot = {:?}\n\
         [pools.main.harness]\nname = \"standin\"\nrun = \"true\"\nmodels = [\"standin\"]\n\
         [pools.main.bands]\nP2 = [{{ model = \"standin\", reasoning = \"low\" }}]\n",
        slots.to_string_lossy()
    ));
    std::fs::write(&config, config_text).unwrap();
    for role in FLOW_ROLES {
        let output = vivi(
            project,
            &[
                "role",
                "set",
                role,
                "--pool",
                "main",
                "--model",
                "P2",
                "--project",
                root,
            ],
        );
        assert!(output.status.success(), "{}", text(&output));
    }
}

#[test]
fn doctor_project_with_flow_role_pools_is_clean() {
    let project = tempfile::tempdir().unwrap();
    init(project.path());
    provision_flow_role_pools(project.path());
    let root = project.path().to_str().unwrap();
    let output = vivi(project.path(), &["doctor", "--project", root, "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    assert_eq!(report["verdict"], "ok");
}

#[test]
fn doctor_fresh_project_warns_for_each_flow_role_without_a_pool() {
    let project = tempfile::tempdir().unwrap();
    init(project.path());
    let root = project.path().to_str().unwrap();
    let output = vivi(project.path(), &["doctor", "--project", root, "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    assert_eq!(report["verdict"], "warn");
    let findings = report["findings"].as_array().expect("findings array");
    for role in FLOW_ROLES {
        let row = format!("pump role {role}");
        assert!(
            findings.iter().any(|finding| {
                finding["section"] == "roles"
                    && finding["name"] == row.as_str()
                    && finding["status"] == "warn"
            }),
            "no warn row {row}: {findings:?}"
        );
    }
}

#[test]
fn doctor_warns_for_a_ready_need_and_json_matches_exit_status() {
    let project = tempfile::tempdir().unwrap();
    init(project.path());
    let root = project.path().to_str().unwrap();
    // A ready need that declares no done_when: the send gate refuses to write
    // one, so it arrives as a raw old-format letter (prose body, no front
    // matter), the one shape the raw delivery gate still admits.
    let identities = vivi(
        project.path(),
        &["mailspace", "identity", "list", "--project", root],
    );
    let address = |name: &str| -> String {
        text(&identities)
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name} ")))
            .expect("identity address")
            .trim()
            .to_string()
    };
    let letter = project.path().join("missing-done-when.eml");
    std::fs::write(
        &letter,
        format!(
            "From: {}\r\nTo: {}\r\nSubject: missing done_when\r\nX-Vivi-Kind: need\r\n\r\n\
             a ready need without a done_when entry\n",
            address("mind"),
            address("cto"),
        ),
    )
    .unwrap();
    let sent = vivi(
        project.path(),
        &[
            "mail",
            "deliver",
            letter.to_str().unwrap(),
            "--folder",
            "needs",
            "--project",
            root,
        ],
    );
    assert!(sent.status.success(), "{}", text(&sent));

    // Keep SQLite's required WAL index present so doctor can use its
    // snapshot-correct read-only connection without creating a sidecar.
    let connection = rusqlite::Connection::open(project.path().join(".vivi/mail.sqlite")).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let output = vivi(project.path(), &["doctor", "--project", root, "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    assert_eq!(report["verdict"], "warn");
    let findings = report["findings"].as_array().expect("findings array");
    assert!(findings.iter().any(|finding| {
        finding["section"] == "mailspace"
            && finding["name"] == "done_when"
            && finding["status"] == "warn"
            && finding["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("ready item"))
    }));

    let output = vivi(
        project.path(),
        &["doctor", "--project", root, "--fix-hints"],
    );
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("  fix:"));
}

#[test]
fn malformed_config_fails_with_json_and_never_echoes_secret_source() {
    let project = tempfile::tempdir().unwrap();
    init(project.path());
    std::fs::write(
        project.path().join(".vivi/mailspace.toml"),
        "name = [invalid\napi_key = 'SECRETTOKEN123'\n",
    )
    .unwrap();
    let root = project.path().to_str().unwrap();
    let output = vivi(project.path(), &["doctor", "--project", root, "--json"]);
    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    assert_eq!(report["verdict"], "fail");
    assert!(!text(&output).contains("SECRETTOKEN123"));

    let output = vivi(project.path(), &["doctor", "--project", root]);
    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert!(!text(&output).contains("SECRETTOKEN123"));
}
