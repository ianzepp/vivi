//! A reply on a task the pump sent to itself, through the real binary and a
//! real mailspace: a bare `vivi mail reply` has no other identity to address,
//! so the pump passes `--to` (see `pump::record::reply_on_task`).

use std::process::{Command, Output};

use vivi::letter::render;
use vivi::letter::work::TaskFront;
use vivi::mailspace::{Mailspace, SendRequest};

fn project() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    (tmp, mailspace)
}

/// A task sent from `mind` to `mind`, as a flow step is.
fn self_task(mailspace: &Mailspace) -> String {
    mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "step".into(),
            body: render(&TaskFront::step("n1", 1, "plan"), "").unwrap(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

fn vivi(tmp: &tempfile::TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .args(["--project", tmp.path().to_str().unwrap()])
        .output()
        .unwrap()
}

fn reply(tmp: &tempfile::TempDir, task: &str, extra: &[&str]) -> Output {
    reply_with(tmp, task, extra, "pump-brief: j1\nthe brief\n")
}

fn reply_with(tmp: &tempfile::TempDir, task: &str, extra: &[&str], text: &str) -> Output {
    let body = tmp.path().join("body.txt");
    std::fs::write(&body, text).unwrap();
    let mut args = vec!["mail", "reply", task, "--from", "mind"];
    args.extend_from_slice(extra);
    args.extend(["--body-file", body.to_str().unwrap()]);
    vivi(tmp, &args)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_reply_without_a_recipient_on_a_pump_to_pump_task_fails() {
    let (tmp, mailspace) = project();
    let task = self_task(&mailspace);

    let out = reply(&tmp, &task, &[]);

    assert!(!out.status.success(), "{}", text(&out.stdout));
    assert!(
        text(&out.stderr).contains("no other local identity or remote address"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn a_reply_to_the_pump_identity_lands_on_the_task_thread() {
    let (tmp, mailspace) = project();
    let task = self_task(&mailspace);

    let out = reply(&tmp, &task, &["--to", "mind"]);

    assert!(out.status.success(), "{}", text(&out.stderr));
    let thread = vivi(&tmp, &["mail", "thread", &task]);
    assert!(thread.status.success(), "{}", text(&thread.stderr));
    assert!(
        text(&thread.stdout).contains("the brief"),
        "{}",
        text(&thread.stdout)
    );
}

const HAND_ENVELOPE: &str =
    "+++\nanswer = \"built\"\nchange = []\ndeviations = []\nremaining = []\n+++\n\nthe report\n";

#[test]
fn a_hand_envelope_reply_is_accepted_and_stored_unchanged() {
    let (tmp, mailspace) = project();
    let task = self_task(&mailspace);

    let out = reply_with(&tmp, &task, &["--to", "mind"], HAND_ENVELOPE);

    assert!(out.status.success(), "{}", text(&out.stderr));
    let thread = vivi(&tmp, &["mail", "thread", &task]);
    assert!(thread.status.success(), "{}", text(&thread.stderr));
    let shown = text(&thread.stdout);
    assert!(shown.contains("answer = \"built\""), "{shown}");
    assert!(shown.contains("the report"), "{shown}");
}

#[test]
fn a_reply_whose_front_matter_fits_no_front_is_refused_and_stores_nothing() {
    let (tmp, mailspace) = project();
    let task = self_task(&mailspace);

    let out = reply_with(
        &tmp,
        &task,
        &["--to", "mind"],
        "+++\ncolour = \"red\"\n+++\n\nthe report\n",
    );

    assert!(!out.status.success(), "{}", text(&out.stdout));
    assert!(
        text(&out.stderr).contains("colour"),
        "{}",
        text(&out.stderr)
    );
    let thread = vivi(&tmp, &["mail", "thread", &task]);
    assert!(
        !text(&thread.stdout).contains("the report"),
        "{}",
        text(&thread.stdout)
    );
}

#[test]
fn a_hand_envelope_with_one_extra_key_is_refused_naming_it_and_stores_nothing() {
    let (tmp, mailspace) = project();
    let task = self_task(&mailspace);

    let out = reply_with(
        &tmp,
        &task,
        &["--to", "mind"],
        "+++\nanswer = \"built\"\ntask = \"x\"\nchange = []\n+++\n\nthe report\n",
    );

    assert!(!out.status.success(), "{}", text(&out.stdout));
    let err = text(&out.stderr);
    assert!(err.contains("hand envelope"), "{err}");
    assert!(err.contains("unknown field `task`"), "{err}");
    let thread = vivi(&tmp, &["mail", "thread", &task]);
    assert!(
        !text(&thread.stdout).contains("the report"),
        "{}",
        text(&thread.stdout)
    );
}
