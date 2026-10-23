//! `vivi pump unlock <need> [--entry] [--note]` through the real binary: it
//! posts one Unlock action on the need's open pump record, and refuses
//! without a record.

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

fn need(mailspace: &Mailspace) -> String {
    mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "item".into(),
            body: "done_when: it works".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

fn unlock(tmp: &tempfile::TempDir, need: &str, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(["pump", "unlock", need])
        .args(extra)
        .args(["--project", tmp.path().to_str().unwrap()])
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn unlock_posts_the_entry_on_the_open_record() {
    let (tmp, mailspace) = project();
    let need = need(&mailspace);
    mailspace.pump_send(None, &need, None).unwrap();

    let out = unlock(&tmp, &need, &["--entry", "slice", "--note", "fixed it"]);

    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("unlock posted"),
        "{}",
        text(&out.stdout)
    );
    let pending = mailspace.pump_pending_actions(&BTreeSet::new()).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].need, need);
    assert_eq!(pending[0].request.kind, PumpActionKind::Unlock);
    assert_eq!(pending[0].request.word.as_deref(), Some("slice"));
    assert_eq!(pending[0].request.reason, "fixed it");
}

#[test]
fn unlock_without_entry_posts_no_word() {
    let (tmp, mailspace) = project();
    let need = need(&mailspace);
    mailspace.pump_send(None, &need, None).unwrap();

    let out = unlock(&tmp, &need, &[]);

    assert!(out.status.success(), "{}", text(&out.stderr));
    let pending = mailspace.pump_pending_actions(&BTreeSet::new()).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request.word, None);
}

#[test]
fn unlock_is_refused_without_an_open_record() {
    let (tmp, mailspace) = project();
    let need = need(&mailspace);

    let out = unlock(&tmp, &need, &[]);

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
