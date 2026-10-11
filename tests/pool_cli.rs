//! `vivi pool dispatch | wait | reconcile`: the refusals that must happen
//! before any machine or network is touched.

use std::path::Path;
use std::process::{Command, Output};

const POOL: &str = r#"
[pool]
repo = "acme/widgets"
slots = 1
image = "debian:12"
build_dir = "target"

[pool.fly]
app = "no-such-app"

[pool.github]
app_id = 1
private_key = "/nonexistent.pem"

[pool.harness]
name = "fake"
run = "fake --model {model} @{brief}"
models = ["m/ok"]
env = ["SOME_KEY"]

[pool.committer]
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
    assert!(said.contains("not in [pool.harness].models"), "{said}");
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
fn old_style_provider_keys_in_pool_are_refused_with_the_new_location() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    setup(project.path());
    let config = project.path().join(".vivi/mailspace.toml");
    let text_in = std::fs::read_to_string(&config)
        .unwrap()
        .replace("[pool.fly]\napp = \"no-such-app\"\n", "")
        .replace("[pool]\n", "[pool]\nregion = \"iad\"\n");
    std::fs::write(&config, text_in).unwrap();
    let out = vivi(["pool", "status", "--project", root]);
    assert!(!out.status.success());
    let said = text(&out);
    assert!(
        said.contains("region") && said.contains("[pool.fly]"),
        "{said}"
    );
}
