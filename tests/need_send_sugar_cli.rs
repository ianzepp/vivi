//! `need|want|task send` front matter flags build the letter's front matter.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

const NO_DONE_WHEN: &str =
    "need has no done_when: pass --done-when or add done_when = [...] to the front matter";

#[test]
fn need_flags_store_front_matter_and_dispatch_with_two_clauses() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();

    let sent = vivi(&send_args(
        "need",
        root,
        &["--done-when", "a", "--done-when", "b", "--target", "vivi"],
    ));
    assert_success(&sent);
    let need = handle_after(&stdout(&sent), "created cto");

    let shown = stdout(&vivi(&["need", "show", &need, "--project", root]));
    assert!(shown.contains("+++"), "{shown}");
    assert!(shown.contains("done_when = [\"a\", \"b\"]"), "{shown}");
    assert!(shown.contains("target = \"vivi\""), "{shown}");
    assert!(shown.contains("prose body"), "{shown}");

    let step = vivi(&["step", "--project", root, "--json"]);
    assert_success(&step);
    let manifest: Value = serde_json::from_str(&stdout(&step)).unwrap();
    let dispatch = manifest["dispatches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["item"] == need.as_str())
        .unwrap_or_else(|| panic!("no dispatch for {need}: {manifest}"));
    assert_eq!(dispatch["done_when_clauses"], 2, "{manifest}");
}

#[test]
fn whole_letter_body_file_with_done_when_is_refused() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();
    let file = project.path().join("letter.md");
    std::fs::write(&file, "+++\ndone_when = [\"x\"]\n+++\n\nprose\n").unwrap();
    let file = file.to_str().unwrap();

    let refused = vivi(&[
        "need",
        "send",
        "--project",
        root,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "s",
        "--body-file",
        file,
        "--done-when",
        "y",
    ]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused)
            .contains("put done_when in its front matter instead of passing --done-when"),
        "{}",
        stderr(&refused)
    );
    assert!(need_list(root).is_empty());

    let accepted = vivi(&[
        "need",
        "send",
        "--project",
        root,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "s",
        "--body-file",
        file,
    ]);
    assert_success(&accepted);
    assert!(
        !stderr(&accepted).contains("note:"),
        "{}",
        stderr(&accepted)
    );
}

#[test]
fn need_without_done_when_is_refused_and_nothing_is_stored() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();

    let refused = vivi(&send_args("need", root, &["--target", "x"]));
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains(NO_DONE_WHEN),
        "{}",
        stderr(&refused)
    );
    assert!(need_list(root).is_empty());
}

#[test]
fn want_question_flag_stores_the_question() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();

    let sent = vivi(&send_args("want", root, &["--question", "q"]));
    assert_success(&sent);
    let want = handle_after(&stdout(&sent), "created cto");
    let shown = stdout(&vivi(&["want", "show", &want, "--project", root]));
    assert!(shown.contains("question = \"q\""), "{shown}");
}

#[test]
fn task_flags_store_front_matter() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();

    let sent = vivi(&send_args("task", root, &["--done-when", "d"]));
    assert_success(&sent);
    let task = handle_after(&stdout(&sent), "created cto");
    let shown = stdout(&vivi(&["task", "show", &task, "--project", root]));
    assert!(shown.contains("done_when = [\"d\"]"), "{shown}");
    assert!(shown.contains("prose body"), "{shown}");
}

fn send_args<'a>(kind: &'a str, root: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
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
        "s",
        "--body",
        "prose body",
    ];
    args.extend_from_slice(extra);
    args
}

fn need_list(root: &str) -> Vec<Value> {
    let listed = vivi(&["need", "list", "--for", "cto", "--json", "--project", root]);
    assert_success(&listed);
    serde_json::from_str::<Value>(&stdout(&listed))
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

fn handle_after(output: &str, prefix: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("missing '{prefix}' in output:\n{output}"))
        .to_string()
}

fn init_roster(project: &Path) {
    let root = project.to_str().unwrap();
    assert_success(&vivi(&["mailspace", "init", "--project", root]));
    for identity in ["ceo", "cto"] {
        assert_success(&vivi(&[
            "mailspace",
            "identity",
            "add",
            identity,
            "--project",
            root,
        ]));
    }
}

fn vivi(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn need_serves_a_want_stores_its_message_id_and_refuses_a_task() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();
    let want = handle_after(
        &stdout(&vivi_ok(&send_args("want", root, &["--question", "q"]))),
        "created cto",
    );
    let task = handle_after(
        &stdout(&vivi_ok(&send_args("task", root, &["--done-when", "d"]))),
        "created cto",
    );

    let sent = vivi_ok(&send_args(
        "need",
        root,
        &["--done-when", "d", "--serves", &want],
    ));
    let need = handle_after(&stdout(&sent), "created cto");
    let shown = stdout(&vivi(&["need", "show", &need, "--project", root]));
    let serves = shown
        .lines()
        .find_map(|line| line.strip_prefix("serves = \""))
        .unwrap_or_else(|| panic!("no serves in:\n{shown}"));
    assert!(serves.starts_with('<') && serves.contains("@"), "{shown}");
    assert!(serves.trim_end().ends_with(">\""), "{shown}");

    let refused = vivi(&send_args(
        "need",
        root,
        &["--done-when", "d", "--serves", &task],
    ));
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains(&format!("{task} is a task, not a want")),
        "{}",
        stderr(&refused)
    );

    let explores = vivi(&send_args(
        "need",
        root,
        &["--done-when", "d", "--explores", &want],
    ));
    assert_success(&explores);
}

fn vivi_ok(args: &[&str]) -> Output {
    let output = vivi(args);
    assert_success(&output);
    output
}
