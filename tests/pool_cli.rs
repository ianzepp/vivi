//! `vivi pool dispatch | wait | reconcile`: the refusals that must happen
//! before any machine or network is touched.

use std::path::Path;
use std::process::{Command, Output};

const POOL: &str = r#"
[pools.lanes]
repo = "acme/widgets"
slots = 1
image = "debian:12"
build_dir = "target"

[pools.lanes.fly]
app = "no-such-app"

[pools.lanes.github]
app_id = 1
private_key = "/nonexistent.pem"

[pools.lanes.harness]
name = "fake"
run = "fake --model {model} @{brief}"
models = ["m/ok"]
env = ["SOME_KEY"]

[pools.lanes.committer]
name = "bot"
email = "bot@example.com"
"#;

fn vivi<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        // A machine call would fail loudly rather than reach a real account.
        .env("FLY_API_TOKEN", "unused")
        .args(args)
        .output()
        .unwrap()
}

fn setup(project: &Path) -> String {
    let root = project.to_str().unwrap();
    vivi(["mailspace", "init", "--project", root]);
    for identity in ["mind", "hand"] {
        vivi(["mailspace", "identity", "add", identity, "--project", root]);
    }
    let config = project.join(".vivi/mailspace.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(POOL);
    std::fs::write(&config, text).unwrap();
    let sent = vivi([
        "task",
        "send",
        "--project",
        root,
        "--from",
        "mind",
        "--to",
        "hand",
        "--subject",
        "Edge case",
        "--body",
        "do it\nfrom: main\nbranch: factory/x\n",
    ]);
    let out = String::from_utf8_lossy(&sent.stdout).into_owned();
    out.lines()
        .find_map(|l| l.strip_prefix("created hand "))
        .and_then(|r| r.split_whitespace().next())
        .unwrap_or_else(|| panic!("no handle in:\n{out}"))
        .to_string()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn pool_set_creates_a_missing_nested_provider_table() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    let initialized = vivi(["mailspace", "init", "--project", root]);
    assert!(initialized.status.success(), "{}", text(&initialized));

    let added = vivi([
        "pool",
        "add",
        "fly-smoke",
        "--provider",
        "fly",
        "--slots",
        "1",
        "--project",
        root,
    ]);
    assert!(added.status.success(), "{}", text(&added));

    let set = vivi([
        "pool",
        "set",
        "fly-smoke",
        "fly.app",
        "smoke-app",
        "--project",
        root,
    ]);
    assert!(set.status.success(), "{}", text(&set));

    let config: toml::Value = toml::from_str(
        &std::fs::read_to_string(project.path().join(".vivi/mailspace.toml")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        config["pools"]["fly-smoke"]["fly"]["app"].as_str(),
        Some("smoke-app")
    );
}

#[test]
fn dispatch_refuses_an_unlisted_model_before_touching_anything() {
    let project = tempfile::tempdir().unwrap();
    let handle = setup(project.path());
    let out = vivi([
        "pool",
        "dispatch",
        &handle,
        "--model",
        "m/other",
        "--project",
        project.path().to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    let said = text(&out);
    assert!(said.contains("not in [harness].models"), "{said}");
    assert!(said.contains("m/ok"), "{said}");
}

#[test]
fn dispatch_needs_a_model_flag() {
    let project = tempfile::tempdir().unwrap();
    let handle = setup(project.path());
    let out = vivi([
        "pool",
        "dispatch",
        &handle,
        "--project",
        project.path().to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(text(&out).contains("--model"), "{}", text(&out));
}

#[test]
fn dispatch_needs_from_and_branch_one_way_or_the_other() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    setup(project.path());
    let sent = vivi([
        "task",
        "send",
        "--project",
        root,
        "--from",
        "mind",
        "--to",
        "hand",
        "--subject",
        "No refs",
        "--body",
        "just do it",
    ]);
    let out = String::from_utf8_lossy(&sent.stdout).into_owned();
    let handle = out
        .lines()
        .find_map(|l| l.strip_prefix("created hand "))
        .and_then(|r| r.split_whitespace().next())
        .unwrap()
        .to_string();
    let out = vivi([
        "pool",
        "dispatch",
        &handle,
        "--model",
        "m/ok",
        "--project",
        root,
    ]);
    assert!(!out.status.success());
    assert!(text(&out).contains("no from"), "{}", text(&out));
}

#[test]
fn dispatch_refuses_a_note_that_is_not_a_task() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    setup(project.path());
    let out = vivi([
        "pool",
        "dispatch",
        "zzzzzzzz",
        "--model",
        "m/ok",
        "--project",
        root,
    ]);
    assert!(!out.status.success());
}

#[test]
fn wait_with_nothing_dispatched_says_so() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    setup(project.path());
    let wait = vivi(["pool", "wait", "--timeout", "1", "--project", root]);
    assert!(!wait.status.success());
    assert!(
        text(&wait).contains("no open dispatches"),
        "{}",
        text(&wait)
    );
}

#[test]
fn doctor_reports_each_side_and_exits_nonzero_when_a_check_fails() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    setup(project.path());
    // An empty PATH makes the machine-host tool unfindable, and the App key
    // does not exist: both sides fail without touching a network.
    let empty = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .env("PATH", empty.path())
        .args(["pool", "doctor", "--json", "--project", root])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["ok"], false);
    let status = |name: &str| {
        json["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .map(|c| c["status"].as_str().unwrap().to_string())
            .unwrap_or_default()
    };
    assert_eq!(status("flyctl"), "fail");
    assert_eq!(status("login"), "skipped");
    assert_eq!(status("app key"), "fail");
    assert_eq!(status("token"), "skipped");
    assert_eq!(status("config"), "ok");
    assert_eq!(status("slots"), "skipped");
    assert!(text(&out).contains("check(s) failed"));
}

#[test]
fn the_retired_single_pool_table_is_refused_naming_the_new_form() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    setup(project.path());
    let config = project.path().join(".vivi/mailspace.toml");
    let mut old = std::fs::read_to_string(&config).unwrap();
    old.push_str("\n[pool]\nslots = 1\n");
    std::fs::write(&config, old).unwrap();
    for args in [
        ["pool", "status"],
        ["pool", "doctor"],
        ["pool", "reconcile"],
    ] {
        let out = vivi([args[0], args[1], "--project", root]);
        assert!(!out.status.success(), "{args:?}");
        let said = text(&out);
        assert!(
            said.contains("[pool]") && said.contains("[pools.<name>]"),
            "{said}"
        );
    }
}

#[test]
fn a_fly_pool_without_an_app_is_refused_naming_the_setting() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    setup(project.path());
    let config = project.path().join(".vivi/mailspace.toml");
    let text_in = std::fs::read_to_string(&config)
        .unwrap()
        .replace("[pools.lanes.fly]\napp = \"no-such-app\"\n", "");
    std::fs::write(&config, text_in).unwrap();
    let out = vivi(["pool", "status", "--project", root]);
    assert!(!out.status.success());
    let said = text(&out);
    assert!(
        said.contains("[pools.lanes]") && said.contains("fly.app is required"),
        "{said}"
    );
}

#[test]
fn a_docker_pool_reports_a_missing_docker_binary_in_the_doctor_and_the_status() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    setup(project.path());
    let config = project.path().join(".vivi/mailspace.toml");
    let swapped = std::fs::read_to_string(&config)
        .unwrap()
        .replace(
            "[pools.lanes.fly]\napp = \"no-such-app\"\n",
            "[pools.lanes.docker]\nmemory = \"1g\"\n",
        )
        .replace("[pools.lanes]\n", "[pools.lanes]\nprovider = \"docker\"\n");
    std::fs::write(&config, swapped).unwrap();
    let empty = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_vivi"))
            .env("PATH", empty.path())
            .args(args)
            .output()
            .unwrap()
    };
    let doctor = run(&["pool", "doctor", "--project", root]);
    let said = text(&doctor);
    assert!(!doctor.status.success());
    assert!(said.contains("Provider (Docker)"), "{said}");
    assert!(said.contains("is Docker installed and on PATH"), "{said}");
    assert!(said.contains("fix: install Docker"), "{said}");
    assert!(
        said.contains("skipped") && said.contains("needs the docker binary"),
        "{said}"
    );
    let status = run(&["pool", "status", "--project", root]);
    assert!(!status.status.success());
    assert!(
        text(&status).contains("is Docker installed and on PATH"),
        "{}",
        text(&status)
    );
}

#[test]
fn porcelain_and_json_together_are_a_usage_error() {
    for command in ["status", "doctor"] {
        let out = vivi(["pool", command, "--porcelain", "--json"]);
        assert_eq!(out.status.code(), Some(2), "{command}");
        let said = text(&out);
        assert!(
            said.contains("--porcelain") && said.contains("--json"),
            "{said}"
        );
    }
}

#[test]
fn doctor_porcelain_prints_only_check_and_summary_lines_with_the_same_exit_code() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    setup(project.path());
    let empty = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .env("PATH", empty.path())
        .args(["pool", "doctor", "--porcelain", "--project", root])
        .output()
        .unwrap();
    assert!(!out.status.success(), "a failed check still exits nonzero");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.len() > 2, "{stdout}");
    for line in &lines[..lines.len() - 1] {
        assert!(line.starts_with("check lanes "), "{line}");
    }
    assert!(
        lines[lines.len() - 1].starts_with("summary lanes "),
        "{stdout}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("check lanes fail ") && l.contains(" flyctl ")),
        "{stdout}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("check(s) failed"), "{stderr}");
}

#[test]
fn wait_porcelain_prints_nothing_on_standard_output_when_it_fails() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    setup(project.path());
    let wait = vivi([
        "pool",
        "wait",
        "--porcelain",
        "--timeout",
        "1",
        "--project",
        root,
    ]);
    assert!(!wait.status.success());
    assert!(
        wait.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&wait.stdout)
    );
}

const DOCKER_ONLY: &str = "provider = \"docker\"\nslots = 4\nimage = \"ubuntu:24.04\"\n";

/// A mailspace with a pool `local` (provider settings only) and, when
/// `second`, a second pool `lanes` of the same kind.
fn two_pools(project: &Path, second: bool) -> String {
    let handle = setup(project);
    let config = project.join(".vivi/mailspace.toml");
    let mut text_in = std::fs::read_to_string(&config).unwrap();
    let start = text_in.find("\n[pools.lanes]").unwrap();
    text_in.truncate(start);
    text_in.push_str(&format!("\n[pools.local]\n{DOCKER_ONLY}"));
    if second {
        text_in.push_str(&format!("\n[pools.lanes]\n{DOCKER_ONLY}"));
    }
    std::fs::write(&config, text_in).unwrap();
    handle
}

fn without_tools(args: &[&str]) -> Output {
    let empty = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .env("PATH", empty.path())
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn with_several_pools_the_commands_that_act_on_one_require_the_flag_and_list_the_names() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    let handle = two_pools(project.path(), true);
    let cases: [Vec<&str>; 5] = [
        vec!["pool", "create"],
        vec!["pool", "destroy", "--all", "--yes"],
        vec!["pool", "dispatch", &handle, "--model", "m/ok"],
        vec!["pool", "scale", "1"],
        vec!["pool", "doctor", "--test"],
    ];
    for mut args in cases {
        args.extend(["--project", root]);
        let out = without_tools(&args);
        assert!(!out.status.success(), "{args:?}");
        let said = text(&out);
        assert!(
            said.contains("several pools (lanes, local)") && said.contains("--pool"),
            "{args:?}: {said}"
        );
    }
}

#[test]
fn one_pool_needs_no_flag_and_an_unknown_name_lists_the_pools() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    two_pools(project.path(), false);
    let create = without_tools(&["pool", "create", "--project", root]);
    assert!(!create.status.success());
    assert!(
        !text(&create).contains("--pool"),
        "a single pool is chosen: {}",
        text(&create)
    );
    let named = without_tools(&["pool", "status", "--pool", "nope", "--project", root]);
    assert!(!named.status.success());
    assert!(
        text(&named).contains("unknown pool \"nope\" (pools: local)"),
        "{}",
        text(&named)
    );
}

#[test]
fn a_provider_only_pool_can_reach_machine_setup_without_a_repo_but_dispatch_needs_one() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    let handle = two_pools(project.path(), false);
    let create = without_tools(&["pool", "create", "--project", root]);
    assert!(!create.status.success());
    assert!(
        text(&create).contains("failed to run `docker`"),
        "{}",
        text(&create)
    );
    assert!(
        !text(&create).contains("repo is required"),
        "{}",
        text(&create)
    );
    let scale = without_tools(&["pool", "scale", "2", "--project", root]);
    assert!(!scale.status.success());
    assert!(
        text(&scale).contains("failed to run `docker`"),
        "{}",
        text(&scale)
    );
    assert!(
        !text(&scale).contains("repo is required"),
        "{}",
        text(&scale)
    );
    let dispatch = without_tools(&[
        "pool",
        "dispatch",
        &handle,
        "--model",
        "m",
        "--project",
        root,
    ]);
    assert!(!dispatch.status.success());
    assert!(
        text(&dispatch).contains("[pools.local] repo is required to dispatch")
            || text(&dispatch).contains("[pools.local.harness] is required"),
        "{}",
        text(&dispatch)
    );
}

#[test]
fn scale_refuses_a_target_above_the_configured_slots_before_asking_the_host() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    two_pools(project.path(), false);
    let out = without_tools(&["pool", "scale", "5", "--project", root]);
    assert!(!out.status.success());
    assert!(text(&out).contains("at most 4"), "{}", text(&out));
    assert!(
        !text(&out).contains("docker"),
        "no host was asked: {}",
        text(&out)
    );
}

#[test]
fn status_covers_every_pool_and_names_each_one_that_cannot_be_listed() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    two_pools(project.path(), true);
    let all = without_tools(&["pool", "status", "--project", root]);
    assert!(!all.status.success());
    let said = text(&all);
    assert!(
        said.contains("pool lanes:") && said.contains("pool local:"),
        "{said}"
    );
    let one = without_tools(&["pool", "status", "--pool", "local", "--project", root]);
    let said = text(&one);
    assert!(
        said.contains("pool local:") && !said.contains("pool lanes:"),
        "{said}"
    );
}

#[test]
fn a_provider_only_pool_passes_doctor_config_and_skips_the_repository() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    two_pools(project.path(), false);
    let out = without_tools(&["pool", "doctor", "--json", "--project", root]);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let find = |name: &str| {
        json["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("no {name}: {json}"))
    };
    assert_eq!(find("config")["status"], "ok");
    assert_eq!(find("config")["pool"], "local");
    assert_eq!(find("repository")["status"], "skipped");
    assert_eq!(
        find("repository")["detail"],
        "no repository configured for this pool"
    );
}
