//! Batched absorb validates first, then keeps each record's event/export behavior.
use std::process::{Command, Output};

use vivi_core::mailspace::{Mailspace, SendRequest};

fn fixture() -> (tempfile::TempDir, Mailspace, Vec<String>) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("sender").unwrap();
    mailspace.add_identity("reader").unwrap();
    let handles = (0..3)
        .map(|i| {
            mailspace
                .send(SendRequest {
                    from: "sender".into(),
                    to: vec!["reader".into()],
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: format!("absorb {i}"),
                    body: "batch body".into(),
                    role: "inbox".into(),
                    kind: None,
                    reply_to: None,
                })
                .unwrap()
                .delivered[0]
                .handle
                .clone()
        })
        .collect();
    (tmp, mailspace, handles)
}

fn absorb(root: &std::path::Path, handles: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args([
            "mail",
            "absorb",
            "--for",
            "reader",
            "--note",
            "processed",
            "--project",
        ])
        .arg(root)
        .args(handles)
        .output()
        .unwrap()
}

#[test]
fn batch_absorb_seals_each_named_record_and_exports_each_once() {
    let (tmp, mut mailspace, handles) = fixture();
    let archive = tmp.path().join("archive");
    std::fs::create_dir_all(archive.join(".git")).unwrap();
    mailspace
        .set_archive(Some(archive.to_string_lossy().into_owned()))
        .unwrap();
    let output = absorb(tmp.path(), &handles);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let storage = mailspace.storage().unwrap();
    for handle in &handles {
        assert!(stdout.contains(&format!("absorbed {handle}\n")), "{stdout}");
        let id = storage.resolve_message_token(handle).unwrap();
        let row = storage.message_by_id(&id).unwrap().unwrap();
        assert!(row.absorbed_at.is_some());
        assert_eq!(row.absorbed_by.as_deref(), Some("reader"));
        assert!(row.read_state);
        let events = storage.list_mailspace_events(&id).unwrap();
        let seals: Vec<_> = events
            .iter()
            .filter(|event| event.command == "mail absorb")
            .collect();
        assert_eq!(seals.len(), 1);
        assert_eq!(seals[0].note.as_deref(), Some("processed"));
        let path = archive
            .join(&mailspace.config.name)
            .join("mail")
            .join(format!("{id}.md"));
        let rendered = std::fs::read_to_string(path).unwrap();
        assert!(rendered.contains("batch body"));
        assert!(rendered.contains("absorbed_by = \"reader\""));
    }
    assert!(absorb(tmp.path(), &handles).status.success());
    for handle in &handles {
        let id = storage.resolve_message_token(handle).unwrap();
        assert_eq!(
            storage
                .list_mailspace_events(&id)
                .unwrap()
                .iter()
                .filter(|event| event.command == "mail absorb")
                .count(),
            1
        );
    }
}

#[test]
fn unknown_handle_rejects_whole_batch_without_sealing_other_records() {
    let (tmp, mailspace, handles) = fixture();
    let mut bad = handles.clone();
    bad.insert(1, "not-a-message".into());
    let output = absorb(tmp.path(), &bad);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("message not found"));
    let storage = mailspace.storage().unwrap();
    for handle in &handles {
        let id = storage.resolve_message_token(handle).unwrap();
        let row = storage.message_by_id(&id).unwrap().unwrap();
        assert!(row.absorbed_at.is_none());
        assert!(!row.read_state);
        assert!(
            !storage
                .list_mailspace_events(&id)
                .unwrap()
                .iter()
                .any(|event| event.command == "mail absorb")
        );
    }
    // Original single-handle syntax still works.
    assert!(absorb(tmp.path(), &handles[..1]).status.success());
    // A variadic positional still requires at least one handle.
    assert!(!absorb(tmp.path(), &[]).status.success());
}
