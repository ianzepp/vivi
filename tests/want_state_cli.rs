//! `want show` prints the computed state and each serving need.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

#[test]
fn want_show_reports_state_and_serving_needs() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();

    let want = send(root, "want", "the want", &["--question", "q"]);
    let alone = stdout(&vivi_ok(&["want", "show", &want, "--project", root]));
    assert!(
        alone.contains("  state    open (no serving needs)"),
        "{alone}"
    );

    let first = send(
        root,
        "need",
        "first need",
        &["--done-when", "a", "--serves", &want],
    );
    let second = send(
        root,
        "need",
        "second need",
        &["--done-when", "b", "--serves", &want],
    );
    vivi_ok(&["need", "done", &first, "--for", "cto", "--project", root]);

    let shown = stdout(&vivi_ok(&["want", "show", &want, "--project", root]));
    assert!(
        shown.contains("  state    open (1 of 2 serving needs done)"),
        "{shown}"
    );
    assert!(
        shown.contains(&format!("  serves   {first}  done")),
        "{shown}"
    );
    assert!(shown.contains("first need"), "{shown}");
    assert!(
        shown.contains(&format!("  serves   {second}  open")),
        "{shown}"
    );
    assert!(shown.contains("second need"), "{shown}");

    let json: Value = serde_json::from_str(&stdout(&vivi_ok(&[
        "want",
        "show",
        &want,
        "--json",
        "--project",
        root,
    ])))
    .unwrap();
    assert!(json["thread"].is_array(), "{json}");
    assert_eq!(json["want_state"]["state"], "open", "{json}");
    let serving = json["want_state"]["serving"].as_array().unwrap();
    assert_eq!(serving.len(), 2, "{json}");
    let done_of = |handle: &str| {
        serving
            .iter()
            .find(|s| s["handle"] == handle)
            .unwrap_or_else(|| panic!("no serving {handle}: {json}"))["done"]
            .clone()
    };
    assert_eq!(done_of(&first), true, "{json}");
    assert_eq!(done_of(&second), false, "{json}");

    vivi_ok(&["need", "done", &second, "--for", "cto", "--project", root]);
    let finished = stdout(&vivi_ok(&["want", "show", &want, "--project", root]));
    assert!(
        finished.contains("  state    done (2 of 2 serving needs done)"),
        "{finished}"
    );
}

fn send(root: &str, kind: &str, subject: &str, extra: &[&str]) -> String {
    let mut args = vec![
        kind,
        "send",
        "--project",
        root,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        subject,
        "--body",
        "prose body",
    ];
    args.extend_from_slice(extra);
    let out = stdout(&vivi_ok(&args));
    out.lines()
        .find_map(|line| line.strip_prefix("created cto"))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("missing 'created cto' in output:\n{out}"))
        .to_string()
}

fn init_roster(project: &Path) {
    let root = project.to_str().unwrap();
    vivi_ok(&["mailspace", "init", "--project", root]);
    for identity in ["ceo", "cto"] {
        vivi_ok(&["mailspace", "identity", "add", identity, "--project", root]);
    }
}

fn vivi_ok(args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status: {}\nargs: {args:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(&output),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}
