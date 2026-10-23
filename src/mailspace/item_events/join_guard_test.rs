//! The join guard blocks a need's join only for a unit a `bound` letter names
//! and this mailspace has never seen.

use super::named_units_unseen;
use crate::mailspace::{Mailspace, RoleUpdate, SendRequest};

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    for role in ["mind", "hand"] {
        mailspace.add_identity(role).unwrap();
    }
    mailspace
        .set_role(
            "mind",
            RoleUpdate {
                kind: Some(Some("mind".into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    (tmp, mailspace)
}

/// An item from `mind` to `hand`; the handle `hand` holds.
fn send_item(mailspace: &Mailspace, role: &str, kind: &str, subject: &str) -> String {
    let result = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: "done_when: ok\n".into(),
            role: role.into(),
            kind: Some(kind.into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    result
        .delivered
        .first()
        .map_or(result.sent, |delivered| delivered.handle.clone())
}

fn rfc_id_of(mailspace: &Mailspace, handle: &str) -> String {
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(handle).unwrap();
    storage
        .message_by_id(&id)
        .unwrap()
        .unwrap()
        .normalized_message_id
        .expect("a Message-ID")
}

/// Hold a `bound` letter as a reply to the need, naming `units`.
fn hold_bound_letter(mailspace: &Mailspace, need: &str, units: &[&str], n: u8) {
    let need_id = rfc_id_of(mailspace, need);
    let unit_headers: String = units
        .iter()
        .map(|unit| format!("X-Vivi-Event-Unit: <{unit}>\r\n"))
        .collect();
    let bytes = format!(
        "From: {}\r\nTo: {}\r\nSubject: Re: need\r\nMessage-ID: <bound-{n}@x.test>\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\nIn-Reply-To: <{need_id}>\r\n\
         X-Vivi-Event: bound\r\nX-Vivi-Event-Of: <{need_id}>\r\n{unit_headers}\r\nbound\r\n",
        mailspace.address_for("mind"),
        mailspace.address_for("hand"),
    )
    .into_bytes();
    let delivered = mailspace.deliver_raw(&bytes, "inbox").unwrap();
    let handle = delivered.delivered.first().map_or_else(
        || delivered.sent.clone().expect("a held copy"),
        |d| d.handle.clone(),
    );
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(&handle).unwrap();
    let content = storage.message_by_id(&id).unwrap().unwrap().content_id;
    crate::sync::link::link_reply(&storage, &content, &need_id).unwrap();
}

fn node_state(mailspace: &Mailspace, handle: &str) -> String {
    mailspace
        .graph_show("backlog")
        .unwrap()
        .nodes
        .into_iter()
        .find(|node| node.source_id == handle)
        .unwrap_or_else(|| panic!("no node for {handle}"))
        .state
}

fn close(mailspace: &Mailspace, handle: &str) {
    mailspace
        .move_task("mind", handle, "done", None, None, &[], &[])
        .unwrap();
}

#[test]
fn a_bound_letter_naming_an_unseen_unit_holds_the_join() {
    let (_tmp, mailspace) = space();
    let need = send_item(&mailspace, "needs", "need", "the need");
    let unit = send_item(&mailspace, "tasks", "task", "the unit");
    mailspace
        .backlog_bind_units(&need, std::slice::from_ref(&unit))
        .unwrap();
    hold_bound_letter(&mailspace, &need, &["never-seen@x.test"], 1);

    assert!(named_units_unseen(&mailspace.storage().unwrap(), &need).unwrap());
    close(&mailspace, &unit);

    assert_eq!(node_state(&mailspace, &unit), "done");
    assert_eq!(
        node_state(&mailspace, &need),
        "open",
        "the unseen unit holds the join"
    );
}

#[test]
fn an_absorbed_named_unit_does_not_block() {
    let (_tmp, mailspace) = space();
    let need = send_item(&mailspace, "needs", "need", "the need");
    let unit = send_item(&mailspace, "tasks", "task", "the unit");
    let unit_id = rfc_id_of(&mailspace, &unit);
    hold_bound_letter(&mailspace, &need, &[&unit_id], 2);
    {
        let storage = mailspace.storage().unwrap();
        let id = storage.resolve_message_token(&unit).unwrap();
        let row = storage.message_by_id(&id).unwrap().unwrap();
        assert!(
            storage
                .absorb_message(&row.account, &row.message_id, &row.account)
                .unwrap()
        );
    }

    assert!(!named_units_unseen(&mailspace.storage().unwrap(), &need).unwrap());
}

#[test]
fn a_local_bind_joins_as_before() {
    let (_tmp, mailspace) = space();
    let need = send_item(&mailspace, "needs", "need", "the need");
    let unit = send_item(&mailspace, "tasks", "task", "the unit");
    mailspace
        .backlog_bind_units(&need, std::slice::from_ref(&unit))
        .unwrap();

    assert!(!named_units_unseen(&mailspace.storage().unwrap(), &need).unwrap());
    close(&mailspace, &unit);

    assert_eq!(node_state(&mailspace, &need), "done", "the join ran");
}
