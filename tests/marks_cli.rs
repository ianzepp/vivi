//! Marks on needs and wants: events, fold, show, list, and the refusals.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use vivi::storage::Storage;

#[test]
fn need_marks_are_events_and_round_trip_through_show_and_list() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();
    let need = send_work(project.path(), "need", "cto", "classify me", "body");

    let marked = vivi([
        "need",
        "mark",
        &need,
        "regression",
        "bug",
        "--for",
        "cto",
        "--project",
        root,
    ]);
    assert_success(&marked);
    let text = stdout(&marked);
    assert!(
        text.contains(&format!("marked {need} regression")),
        "{text}"
    );
    assert!(text.contains(&format!("marked {need} bug")), "{text}");
    assert_eq!(mark_notes(project.path(), &need), vec!["regression", "bug"]);

    let again = vivi([
        "need",
        "mark",
        &need,
        "bug",
        "--for",
        "cto",
        "--project",
        root,
    ]);
    assert_success(&again);
    assert!(
        stdout(&again).contains(&format!("{need} already marked bug")),
        "{}",
        stdout(&again)
    );
    assert_eq!(mark_notes(project.path(), &need).len(), 2);

    let show = stdout(&vivi(["need", "show", &need, "--project", root]));
    assert!(show.contains("Marks: bug regression"), "{show}");
    assert!(show.contains("body"), "{show}");
    let show_json: Value = serde_json::from_str(&stdout(&vivi([
        "need",
        "show",
        "--json",
        &need,
        "--project",
        root,
    ])))
    .unwrap();
    assert_eq!(
        marks_in(&show_json, &need),
        serde_json::json!(["bug", "regression"])
    );

    let list = stdout(&vivi([
        "need",
        "list",
        "--for",
        "cto",
        "--mark",
        "bug",
        "--mark",
        "regression",
        "--project",
        root,
    ]));
    assert!(
        list.contains("handle  status  role  date  from  subject  last_event"),
        "{list}"
    );
    assert!(list.contains(&need), "{list}");
    let missed = stdout(&vivi([
        "need",
        "list",
        "--for",
        "cto",
        "--mark",
        "bug",
        "--mark",
        "absent",
        "--project",
        root,
    ]));
    assert!(!missed.contains(&need), "{missed}");
    let list_json: Value = serde_json::from_str(&stdout(&vivi([
        "need",
        "list",
        "--for",
        "cto",
        "--json",
        "--project",
        root,
    ])))
    .unwrap();
    assert_eq!(
        marks_in(&list_json, &need),
        serde_json::json!(["bug", "regression"])
    );

    let bad = vivi([
        "need",
        "mark",
        &need,
        "bug fix",
        "--for",
        "cto",
        "--project",
        root,
    ]);
    assert!(!bad.status.success());
    assert!(
        stderr(&bad).contains("invalid mark 'bug fix'"),
        "{}",
        stderr(&bad)
    );
    assert_eq!(mark_notes(project.path(), &need).len(), 2);

    let wrong = vivi([
        "need",
        "mark",
        &need,
        "docs",
        "--for",
        "ceo",
        "--project",
        root,
    ]);
    assert!(!wrong.status.success());
    assert!(
        stderr(&wrong).contains("held by identity 'cto'"),
        "{}",
        stderr(&wrong)
    );

    let mixed = vivi([
        "need",
        "mark",
        &need,
        "docs",
        "nope slug",
        "--for",
        "cto",
        "--project",
        root,
    ]);
    assert!(!mixed.status.success());
    assert_eq!(mark_notes(project.path(), &need).len(), 2);

    assert_success(&vivi([
        "need",
        "unmark",
        &need,
        "bug",
        "--for",
        "cto",
        "--project",
        root,
    ]));
    assert!(
        stdout(&vivi([
            "need",
            "unmark",
            &need,
            "bug",
            "--for",
            "cto",
            "--project",
            root
        ]))
        .contains(&format!("{need} already unmarked bug"))
    );
    let storage = Storage::open_mailspace(&project.path().join(".vivi")).unwrap();
    assert!(storage.item_metadata(&need).unwrap().is_empty());
}

#[test]
fn want_mark_survives_promote_and_done() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();
    let want = send_work(project.path(), "want", "cto", "parked bug", "later");
    assert_success(&vivi([
        "want",
        "mark",
        &want,
        "bug",
        "--for",
        "cto",
        "--project",
        root,
    ]));
    let listed = stdout(&vivi([
        "want",
        "list",
        "--for",
        "cto",
        "--mark",
        "bug",
        "--project",
        root,
    ]));
    assert!(
        listed.contains("handle  status  priority  rank  repo  lane  subject"),
        "{listed}"
    );
    assert!(listed.contains(&want), "{listed}");

    assert_success(&vivi([
        "want",
        "promote",
        &want,
        "--for",
        "cto",
        "--project",
        root,
    ]));
    let show = stdout(&vivi(["need", "show", &want, "--project", root]));
    assert!(show.contains("Marks: bug"), "{show}");
    let still_want = vivi([
        "want",
        "mark",
        &want,
        "docs",
        "--for",
        "cto",
        "--project",
        root,
    ]);
    assert!(!still_want.status.success());
    assert!(
        stderr(&still_want).contains("is not a want"),
        "{}",
        stderr(&still_want)
    );

    assert_success(&vivi([
        "need",
        "done",
        &want,
        "--for",
        "cto",
        "--project",
        root,
    ]));
    let done = stdout(&vivi([
        "need",
        "list",
        "--for",
        "cto",
        "--status",
        "done",
        "--mark",
        "bug",
        "--project",
        root,
    ]));
    assert!(done.contains(&want), "{done}");
    assert!(stdout(&vivi(["need", "show", &want, "--project", root])).contains("Marks: bug"));
}

#[test]
fn marks_do_not_change_step_or_bind() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();
    let need = send_work(
        project.path(),
        "need",
        "cto",
        "dispatch me",
        "done_when: the unit lands\n",
    );
    assert_success(&vivi([
        "need",
        "mark",
        &need,
        "bug",
        "--for",
        "cto",
        "--project",
        root,
    ]));
    let step = stdout(&vivi(["step", "--project", root]));
    assert!(step.contains("dispatch me"), "{step}");
    assert!(step.contains("need"), "{step}");

    let task = send_work(
        project.path(),
        "task",
        "cto",
        "the unit",
        "done_when: lands\n",
    );
    assert_success(&vivi(["need", "bind", &need, &task, "--project", root]));
    let lowered = stdout(&vivi(["step", "--project", root]));
    assert!(lowered.contains("lowered_awaiting_units"), "{lowered}");
    let shown = stdout(&vivi(["need", "show", &need, "--project", root]));
    assert!(shown.contains("Marks: bug"), "{shown}");
    assert!(shown.contains(&task), "{shown}");

    assert_success(&vivi([
        "task",
        "done",
        &task,
        "--for",
        "cto",
        "--project",
        root,
    ]));
    let after = stdout(&vivi(["need", "show", &need, "--project", root]));
    assert!(after.contains("Marks: bug"), "{after}");
}

#[test]
fn absorbed_need_refuses_a_new_mark_and_keeps_the_old_one() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();
    let need = send_work(project.path(), "need", "cto", "sealed", "body");
    assert_success(&vivi([
        "need",
        "mark",
        &need,
        "bug",
        "--for",
        "cto",
        "--project",
        root,
    ]));
    assert_success(&vivi([
        "need",
        "absorb",
        &need,
        "--for",
        "cto",
        "--project",
        root,
    ]));
    let refused = vivi([
        "need",
        "unmark",
        &need,
        "bug",
        "--for",
        "cto",
        "--project",
        root,
    ]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("is absorbed and can no longer be changed"),
        "{}",
        stderr(&refused)
    );
    assert!(stdout(&vivi(["need", "show", &need, "--project", root])).contains("Marks: bug"));
}

fn init_roster(project: &Path) {
    assert_success(&vivi([
        "mailspace",
        "init",
        "--project",
        project.to_str().unwrap(),
    ]));
    for identity in ["ceo", "cto"] {
        assert_success(&vivi([
            "mailspace",
            "identity",
            "add",
            identity,
            "--project",
            project.to_str().unwrap(),
        ]));
    }
}

fn send_work(project: &Path, kind: &str, to: &str, subject: &str, body: &str) -> String {
    let output = vivi([
        kind,
        "send",
        "--project",
        project.to_str().unwrap(),
        "--from",
        "ceo",
        "--to",
        to,
        "--subject",
        subject,
        "--body",
        body,
    ]);
    assert_success(&output);
    handle_after(&stdout(&output), &format!("created {to}"))
}

fn marks_in(json: &Value, handle: &str) -> Value {
    json.as_array()
        .and_then(|items| items.iter().find(|item| item["handle"] == handle))
        .map(|item| item["marks"].clone())
        .unwrap_or_else(|| panic!("no marks for {handle} in {json}"))
}

fn mark_notes(project: &Path, handle: &str) -> Vec<String> {
    let storage = Storage::open_mailspace(&project.join(".vivi")).unwrap();
    storage
        .list_mailspace_events(handle)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == "marked" || event.event_type == "unmarked")
        .map(|event| event.note.unwrap_or_default())
        .collect()
}

fn vivi<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
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

fn handle_after(output: &str, prefix: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("missing '{prefix}' in output:\n{output}"))
        .to_string()
}
