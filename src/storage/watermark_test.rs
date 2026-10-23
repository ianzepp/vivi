use crate::letter::fixture::need_body;
use crate::mailspace::{Mailspace, SendRequest};
use crate::storage::{ChangeWatermark, Storage};

fn roster() -> (Mailspace, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    (mailspace, tmp)
}

fn send_need(mailspace: &Mailspace) -> String {
    mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "watched".into(),
            body: need_body(&["done"], "body"),
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

fn watermark(mailspace: &Mailspace) -> ChangeWatermark {
    mailspace.storage().unwrap().change_watermark().unwrap()
}

#[test]
fn an_empty_store_has_the_zero_watermark() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::open_mailspace(tmp.path()).unwrap();
    let mark = storage.change_watermark().unwrap();
    assert_eq!(
        mark,
        ChangeWatermark {
            messages: 0,
            events: 0,
            graph_events: 0
        }
    );
    assert_eq!(mark.to_string(), "0.0.0");
}

#[test]
fn the_watermark_holds_still_when_nothing_happens() {
    let (mailspace, _tmp) = roster();
    send_need(&mailspace);
    let first = watermark(&mailspace);
    assert_eq!(watermark(&mailspace), first);
}

#[test]
fn a_send_moves_the_watermark() {
    let (mailspace, _tmp) = roster();
    let before = watermark(&mailspace);
    send_need(&mailspace);
    let after = watermark(&mailspace);
    assert_ne!(after, before);
    assert!(after.messages > before.messages);
}

#[test]
fn a_done_and_a_reopen_each_move_the_watermark() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace);
    let opened = watermark(&mailspace);
    mailspace
        .move_item("ceo", &need, "done", None, "need done", None)
        .unwrap();
    let closed = watermark(&mailspace);
    assert_ne!(closed, opened);
    mailspace
        .move_item("ceo", &need, "needs", None, "need reopen", None)
        .unwrap();
    assert_ne!(watermark(&mailspace), closed);
}

#[test]
fn a_mark_moves_the_watermark_and_an_unchanged_mark_does_not() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace);
    let before = watermark(&mailspace);
    let slugs = ["bug".to_string()];
    mailspace
        .set_marks("cto", &need, "need", true, &slugs)
        .unwrap();
    let marked = watermark(&mailspace);
    assert_ne!(marked, before);
    assert!(marked.events > before.events);
    mailspace
        .set_marks("cto", &need, "need", true, &slugs)
        .unwrap();
    assert_eq!(watermark(&mailspace), marked);
}

#[test]
fn a_second_connection_sees_the_first_connections_writes() {
    let (mailspace, tmp) = roster();
    let reader = Storage::open_mailspace(&tmp.path().join(".vivi")).unwrap();
    let before = reader.change_watermark().unwrap();
    send_need(&mailspace);
    assert_ne!(reader.change_watermark().unwrap(), before);
}
