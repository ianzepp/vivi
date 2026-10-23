//! `vivi pump answer <step-task> <word> [--note]` through the real binary:
//! it posts on the need's open pump record, and refuses without a record or
//! for a task that is not a pump step.

use std::collections::BTreeSet;
use std::process::{Command, Output};

use vivi::mailspace::{Mailspace, PumpActionKind, SendRequest};

fn project() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("operator").unwrap();
    mailspace.add_identity("mind").unwrap();
    (tmp, mailspace)
}

fn send(mailspace: &Mailspace, role: &str, kind: &str, body: &str) -> String {
    mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "item".into(),
            body: body.into(),
            role: role.into(),
            kind: Some(kind.into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

fn answer(tmp: &tempfile::TempDir, step: &str, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(["pump", "answer", step, "planned"])
        .args(extra)
        .args(["--project", tmp.path().to_str().unwrap()])
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn answer_posts_on_the_open_record() {
    let (tmp, mailspace) = project();
    let need = send(&mailspace, "needs", "need", "done_when: it works");
    mailspace.pump_send(None, &need, None).unwrap();
    let step = send(
        &mailspace,
        "tasks",
        "task",
        &format!("pump-step: {need} r1 plan"),
    );

    let out = answer(&tmp, &step, &["--note", "operator ruled"]);

    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("posted"),
        "{}",
        text(&out.stdout)
    );
    let pending = mailspace.pump_pending_actions(&BTreeSet::new()).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].need, need);
    assert_eq!(pending[0].request.kind, PumpActionKind::Answer);
    assert_eq!(pending[0].request.word.as_deref(), Some("planned"));
    assert_eq!(pending[0].request.reason, "operator ruled");
}

#[test]
fn answer_is_refused_without_an_open_record() {
    let (tmp, mailspace) = project();
    let need = send(&mailspace, "needs", "need", "done_when: it works");
    let step = send(
        &mailspace,
        "tasks",
        "task",
        &format!("pump-step: {need} r1 plan"),
    );

    let out = answer(&tmp, &step, &[]);

    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("no open pump record"),
        "{}",
        text(&out.stderr)
    );
    assert!(
        mailspace
            .pump_pending_actions(&BTreeSet::new())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn answer_is_refused_for_a_task_that_is_not_a_pump_step() {
    let (tmp, mailspace) = project();
    let need = send(&mailspace, "needs", "need", "done_when: it works");
    mailspace.pump_send(None, &need, None).unwrap();
    let task = send(&mailspace, "tasks", "task", "an ordinary task");

    let out = answer(&tmp, &task, &[]);

    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("not a pump step task"),
        "{}",
        text(&out.stderr)
    );
}
