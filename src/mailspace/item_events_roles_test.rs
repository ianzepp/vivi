//! Whose word an arriving event letter is: the From address names a role,
//! and the receiver judges it. A letter from this mailspace's own domain
//! counts only when its role may close the item (the author, or a role of
//! kind `mind` or `operator`); a letter from another member's prefix domain
//! counts unseen; and a legacy name-domain mailspace (no id) still counts
//! its own letters.

use super::*;
use crate::mailspace::RoleUpdate;
use crate::mailspace::SendRequest;
use crate::mailspace::marks::folded_marks;

/// A space whose `mind` is of kind `mind` and whose work is authored by
/// `head` (no kind): the role-kind rule is what lets `mind` close, not
/// authorship.
fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    for role in ["mind", "hand", "auditor", "head"] {
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

/// A task authored by `head` and held by `hand`: `(delivered, sent)`.
fn task_of(mailspace: &Mailspace) -> (String, String) {
    let result = mailspace
        .send(SendRequest {
            from: "head".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "judged by the sender address".into(),
            body: crate::letter::fixture::task_body(""),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap();
    let delivered = result
        .delivered
        .first()
        .map_or_else(|| result.sent.clone(), |d| d.handle.clone());
    (delivered, result.sent)
}

/// The normalized `Message-ID` of the item a handle names.
fn item_id_of(mailspace: &Mailspace, handle: &str) -> String {
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(handle).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    held_item(&storage, &view.content_id)
        .unwrap()
        .expect("the handle names work")
        .rfc_id
}

/// Deliver one crafted event letter through the path `mail deliver` uses,
/// from `from` on the item `item_id`, and settle it: the tally of what the
/// receiver did with it.
fn deliver_event(
    mailspace: &Mailspace,
    from: &str,
    item_id: &str,
    headers: &str,
    n: u8,
) -> EventTally {
    let bytes = format!(
        "From: {from}\r\nTo: {}\r\nSubject: Re: judged by the sender address\r\n\
         Message-ID: <crafted-{n}@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\
         In-Reply-To: <{item_id}>\r\nX-Vivi-Event-Of: <{item_id}>\r\n{headers}\r\ncrafted\r\n",
        mailspace.address_for("hand"),
    )
    .into_bytes();
    let delivered = mailspace.deliver_raw(&bytes, "inbox").unwrap();
    let handle = delivered.delivered.first().map_or_else(
        || delivered.sent.clone().expect("a held copy"),
        |d| d.handle.clone(),
    );
    let content = {
        let storage = mailspace.storage().unwrap();
        let id = storage.resolve_message_token(&handle).unwrap();
        storage.message_by_id(&id).unwrap().unwrap().content_id
    };
    // The reply link the receiving side makes when a letter arrives, and
    // the settle that follows it.
    {
        let storage = mailspace.storage().unwrap();
        crate::sync::link::link_reply(&storage, &content, item_id).unwrap();
    }
    mailspace
        .apply_arrived_events(std::slice::from_ref(&content))
        .unwrap()
}

#[test]
fn an_own_domain_letter_from_a_role_that_may_not_close_closes_nothing() {
    let kinds: [(&str, &str); 5] = [
        ("done", "X-Vivi-Event: done\r\n"),
        ("reopened", "X-Vivi-Event: reopened\r\n"),
        (
            "absorbed",
            "X-Vivi-Event: absorbed\r\nX-Vivi-Event-State: done\r\n",
        ),
        (
            "marked",
            "X-Vivi-Event: marked\r\nX-Vivi-Event-Slug: ready\r\n",
        ),
        (
            "bound",
            "X-Vivi-Event: bound\r\nX-Vivi-Event-Unit: <u1@x.test>\r\n",
        ),
    ];
    for (n, (kind, headers)) in kinds.iter().enumerate() {
        let n = u8::try_from(n).expect("a small index");
        let (_tmp, mailspace) = space();
        let (task, _sent) = task_of(&mailspace);
        let id = item_id_of(&mailspace, &task);

        let tally = deliver_event(&mailspace, &mailspace.address_for("hand"), &id, headers, n);

        assert_eq!(
            tally,
            EventTally {
                applied: 0,
                ignored: 1
            },
            "{kind}"
        );
        assert_eq!(mailspace.item_event_state(&task).unwrap(), None, "{kind}");
        let rows = mailspace.list("hand", "tasks").unwrap();
        assert_eq!(rows.len(), 1, "{kind}: the row never moved");
        assert!(rows[0].absorbed_at.is_none(), "{kind}: no seal");
        let storage = mailspace.storage().unwrap();
        let events = storage.list_mailspace_events(&rows[0].message_id).unwrap();
        assert!(folded_marks(&events).is_empty(), "{kind}: no mark");
    }
}

#[test]
fn an_own_domain_letter_from_an_unknown_role_is_ignored() {
    let (_tmp, mailspace) = space();
    let (task, _sent) = task_of(&mailspace);
    let id = item_id_of(&mailspace, &task);
    let ghost = format!("ghost@{}", mailspace.address_domain());

    let tally = deliver_event(&mailspace, &ghost, &id, "X-Vivi-Event: done\r\n", 9);

    assert_eq!(
        tally,
        EventTally {
            applied: 0,
            ignored: 1
        }
    );
    assert_eq!(mailspace.item_event_state(&task).unwrap(), None);
    assert_eq!(mailspace.list("hand", "tasks").unwrap().len(), 1);
}

#[test]
fn an_own_domain_letter_from_a_mind_kind_role_closes() {
    let (_tmp, mailspace) = space();
    let (task, _sent) = task_of(&mailspace);
    let id = item_id_of(&mailspace, &task);

    let tally = deliver_event(
        &mailspace,
        &mailspace.address_for("mind"),
        &id,
        "X-Vivi-Event: done\r\n",
        1,
    );

    assert_eq!(tally.applied, 1, "ignored: {}", tally.ignored);
    assert_eq!(
        mailspace.item_event_state(&task).unwrap().unwrap().state,
        "done"
    );
    assert_eq!(mailspace.list("hand", "done").unwrap().len(), 1);
}

#[test]
fn a_letter_from_another_members_prefix_domain_counts() {
    let (_tmp, mailspace) = space();
    let (task, _sent) = task_of(&mailspace);
    let id = item_id_of(&mailspace, &task);
    let other = if mailspace.address_prefix() == "3f9a1c7e" {
        "aaaaaaaa"
    } else {
        "3f9a1c7e"
    };
    // A `hand` of the other mailspace: the receiver cannot see that
    // mailspace's roles, so its word counts like any member's.
    let from = format!("hand@{other}.local");

    let tally = deliver_event(&mailspace, &from, &id, "X-Vivi-Event: done\r\n", 2);

    assert_eq!(tally.applied, 1, "ignored: {}", tally.ignored);
    assert_eq!(
        mailspace.item_event_state(&task).unwrap().unwrap().state,
        "done"
    );
    assert_eq!(mailspace.list("hand", "done").unwrap().len(), 1);
}

#[test]
fn a_legacy_name_domain_mailspace_counts_its_own_done_and_refuses_a_second_close() {
    let (_tmp, mut mailspace) = space();
    // A legacy mailspace has no id: its addresses are `<role>@<name>.local`.
    mailspace.config.id = None;
    let (task, sent) = task_of(&mailspace);
    assert_eq!(
        mailspace.address_for("head"),
        format!("head@{}.local", mailspace.config.name)
    );

    mailspace
        .move_task("head", &sent, "done", None, None, &[], &[])
        .unwrap();

    // The locally written done is seen by `item_head`...
    assert_eq!(
        mailspace.item_event_state(&task).unwrap().unwrap().state,
        "done"
    );
    assert_eq!(mailspace.list("hand", "done").unwrap().len(), 1);
    // ...so the already-closed guard refuses a second close.
    let error = mailspace
        .move_task("mind", &task, "done", None, None, &[], &[])
        .unwrap_err();
    assert!(
        error.to_string().contains("already closed by its owner"),
        "{error}"
    );
}
