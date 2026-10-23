//! Q7: the pump identity closes and reopens a need it is running.

use super::super::EventTally;
use super::super::held_item;
use crate::mailspace::{Mailspace, SendRequest};
use crate::pump::config::PumpConfig;

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    for role in ["head", "pumper", "pump", "other", "bystander"] {
        mailspace.add_identity(role).unwrap();
    }
    mailspace.config.pump = Some(PumpConfig {
        from: "pump".into(),
        reply_from: "pumper".into(),
        ..PumpConfig::default()
    });
    (tmp, mailspace)
}

/// A need authored by `head`, addressed to the pump's `reply_from` (`pumper`), whose identity differs from `[pump] from` (`pump`).
fn need_of(mailspace: &Mailspace) -> String {
    mailspace
        .send(SendRequest {
            from: "head".into(),
            to: vec!["pumper".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "run by the pump".into(),
            body: "done_when: ok\n".into(),
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

fn may_close(mailspace: &Mailspace, need: &str, identity: &str) -> bool {
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(need).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    let item = held_item(&storage, &view.content_id).unwrap().unwrap();
    mailspace.may_close(&item, identity)
}

fn has_history(mailspace: &Mailspace, need: &str) -> bool {
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(need).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    held_item(&storage, &view.content_id)
        .unwrap()
        .unwrap()
        .delegation
        .history
}

#[test]
fn the_pump_identity_may_close_a_need_with_an_open_record_only() {
    let (_tmp, mailspace) = space();
    let need = need_of(&mailspace);

    assert!(!may_close(&mailspace, &need, "pumper"), "no open record");

    let record = mailspace.pump_send(Some("pump"), &need, None).unwrap();
    assert!(may_close(&mailspace, &need, "pumper"), "open record");
    assert!(
        !may_close(&mailspace, &need, "pump"),
        "`[pump] from` is not the pump identity"
    );
    assert!(
        !may_close(&mailspace, &need, "bystander"),
        "others gain nothing"
    );
    assert!(
        !may_close(&mailspace, &need, "other"),
        "others gain nothing"
    );

    mailspace.pump_done(&record.handle, None).unwrap();
    assert!(!may_close(&mailspace, &need, "pumper"), "record closed");
}

#[test]
fn the_pump_identity_may_close_a_delegated_need() {
    let (_tmp, mailspace) = space();
    let need = need_of(&mailspace);
    mailspace.assign_need("head", &need, "other").unwrap();
    assert!(has_history(&mailspace, &need));
    assert!(!may_close(&mailspace, &need, "pumper"), "no open record");

    mailspace.pump_send(Some("pump"), &need, None).unwrap();
    assert!(may_close(&mailspace, &need, "pumper"));
    assert!(!may_close(&mailspace, &need, "bystander"));
}

#[test]
fn the_pump_closes_and_reopens_a_need_another_role_authored() {
    let (_tmp, mailspace) = space();
    // Without an open record the close logs no event. That close does move
    // the need to done, and a record needs an open need, so it runs on a
    // need of its own.
    let unrecorded = need_of(&mailspace);
    let refused = mailspace
        .move_item_outcome(
            "pumper",
            &unrecorded,
            "done",
            Some("no record"),
            "need done",
            None,
        )
        .is_ok_and(|_| mailspace.item_event_state(&unrecorded).unwrap().is_some());
    assert!(!refused, "no open record: no close event");

    // The record is opened while the need is open, as the pump does.
    let need = need_of(&mailspace);
    mailspace.pump_send(Some("pump"), &need, None).unwrap();
    mailspace
        .move_item_outcome("pumper", &need, "done", Some("landed"), "need done", None)
        .unwrap();
    assert_eq!(
        mailspace.item_event_state(&need).unwrap().unwrap().state,
        "done"
    );
    mailspace
        .move_item_outcome("pumper", &need, "needs", None, "need reopen", None)
        .unwrap();
    assert_eq!(
        mailspace.item_event_state(&need).unwrap().unwrap().state,
        "reopened"
    );
}

/// Deliver a letter from `from` as a reply on the need, link it, and settle.
fn deliver(mailspace: &Mailspace, from: &str, need: &str, headers: &str, n: u8) -> EventTally {
    deliver_at(
        mailspace,
        from,
        need,
        headers,
        n,
        "Sun, 11 Oct 2026 12:00:00 +0000",
    )
}

/// [`deliver`] with an explicit `Date` header.
fn deliver_at(
    mailspace: &Mailspace,
    from: &str,
    need: &str,
    headers: &str,
    n: u8,
    date: &str,
) -> EventTally {
    let item_id = {
        let storage = mailspace.storage().unwrap();
        let id = storage.resolve_message_token(need).unwrap();
        let view = storage.message_by_id(&id).unwrap().unwrap();
        held_item(&storage, &view.content_id)
            .unwrap()
            .unwrap()
            .rfc_id
    };
    let bytes = format!(
        "From: {from}\r\nTo: {}\r\nSubject: Re: run by the pump\r\n\
         Message-ID: <pump-auth-{n}@x.test>\r\nDate: {date}\r\n\
         In-Reply-To: <{item_id}>\r\n{headers}\r\nbody\r\n",
        mailspace.address_for("pumper"),
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
    {
        let storage = mailspace.storage().unwrap();
        crate::sync::link::link_reply(&storage, &content, &item_id).unwrap();
    }
    mailspace
        .apply_arrived_events(std::slice::from_ref(&content))
        .unwrap()
}

fn done_headers(need_id: &str) -> String {
    format!("X-Vivi-Event: done\r\nX-Vivi-Event-Of: <{need_id}>\r\n")
}

fn need_rfc_id(mailspace: &Mailspace, need: &str) -> String {
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(need).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    held_item(&storage, &view.content_id)
        .unwrap()
        .unwrap()
        .rfc_id
}

#[test]
fn the_pumps_done_still_counts_locally_after_its_record_closes() {
    let (_tmp, mailspace) = space();
    let need = need_of(&mailspace);
    let record = mailspace.pump_send(Some("pump"), &need, None).unwrap();
    mailspace
        .move_item_outcome("pumper", &need, "done", Some("landed"), "need done", None)
        .unwrap();
    mailspace.pump_done(&record.handle, None).unwrap();

    // The close was written while the record was open, so it keeps counting.
    assert_eq!(
        mailspace.item_event_state(&need).unwrap().unwrap().state,
        "done"
    );
    // The closed record grants no new write.
    assert!(!may_close(&mailspace, &need, "pumper"), "record closed");
    assert!(
        mailspace
            .move_item_outcome("pumper", &need, "needs", None, "need reopen", None)
            .is_err(),
        "no new write once the record is done"
    );
    assert_eq!(
        mailspace.item_event_state(&need).unwrap().unwrap().state,
        "done"
    );
}

#[test]
fn a_second_member_accepts_the_pumps_done_after_it_posted_on_the_thread() {
    let (_tmp, mailspace) = space();
    let need = need_of(&mailspace);
    mailspace.assign_need("head", &need, "other").unwrap();
    let rfc = need_rfc_id(&mailspace, &need);
    let other = if mailspace.address_prefix() == "3f9a1c7e" {
        "aaaaaaaa"
    } else {
        "3f9a1c7e"
    };
    let pump = format!("pumper@{other}.local");
    let old_from = format!("pump@{other}.local");
    let stranger = format!("bystander@{other}.local");

    // A done from the pump role that never posted on the thread: ignored.
    let tally = deliver(&mailspace, &pump, &need, &done_headers(&rfc), 1);
    assert_eq!(tally.applied, 0, "no earlier post");
    // A done from a role that is not the pump, even after posting: ignored.
    deliver(&mailspace, &stranger, &need, "", 2);
    let tally = deliver(&mailspace, &stranger, &need, &done_headers(&rfc), 3);
    assert_eq!(tally.applied, 0, "not the pump identity");
    assert_eq!(mailspace.item_event_state(&need).unwrap(), None);

    // The `[pump] from` role is not the pump identity, even after posting.
    deliver(&mailspace, &old_from, &need, "", 6);
    let tally = deliver(&mailspace, &old_from, &need, &done_headers(&rfc), 7);
    assert_eq!(tally.applied, 0, "`from` is not the pump identity");

    // The pump posts a plain notice, then its done travels and counts.
    deliver(&mailspace, &pump, &need, "", 4);
    let tally = deliver(&mailspace, &pump, &need, &done_headers(&rfc), 5);
    assert_eq!(tally.applied, 1, "ignored: {}", tally.ignored);
    assert_eq!(
        mailspace.item_event_state(&need).unwrap().unwrap().state,
        "done"
    );
}

/// Two foreign prefixes, neither this mailspace's own.
fn foreign_prefixes(mailspace: &Mailspace) -> (&'static str, &'static str) {
    let mut free = ["aaaaaaaa", "bbbbbbbb", "cccccccc"]
        .into_iter()
        .filter(|p| *p != mailspace.address_prefix());
    (free.next().unwrap(), free.next().unwrap())
}

#[test]
fn the_first_pump_role_poster_is_the_only_address_that_counts() {
    let (_tmp, mailspace) = space();
    let need = need_of(&mailspace);
    mailspace.assign_need("head", &need, "other").unwrap();
    let rfc = need_rfc_id(&mailspace, &need);
    let (first, later) = foreign_prefixes(&mailspace);
    let first_pump = format!("pumper@{first}.local");
    let later_pump = format!("pumper@{later}.local");

    // The first pump-role post, then a later one from another prefix.
    deliver_at(
        &mailspace,
        &first_pump,
        &need,
        "",
        1,
        "Sun, 11 Oct 2026 12:00:00 +0000",
    );
    deliver_at(
        &mailspace,
        &later_pump,
        &need,
        "",
        2,
        "Sun, 11 Oct 2026 13:00:00 +0000",
    );

    // The later address posted, then says done: it is not the earliest.
    let tally = deliver_at(
        &mailspace,
        &later_pump,
        &need,
        &done_headers(&rfc),
        3,
        "Sun, 11 Oct 2026 14:00:00 +0000",
    );
    assert_eq!(tally.applied, 0, "a later same-role address never counts");
    assert_eq!(mailspace.item_event_state(&need).unwrap(), None);

    // The first poster's done counts.
    let tally = deliver_at(
        &mailspace,
        &first_pump,
        &need,
        &done_headers(&rfc),
        4,
        "Sun, 11 Oct 2026 15:00:00 +0000",
    );
    assert_eq!(tally.applied, 1, "ignored: {}", tally.ignored);
    assert_eq!(
        mailspace.item_event_state(&need).unwrap().unwrap().state,
        "done"
    );
}

#[test]
fn a_pump_role_done_with_no_earlier_post_does_not_count() {
    let (_tmp, mailspace) = space();
    let need = need_of(&mailspace);
    mailspace.assign_need("head", &need, "other").unwrap();
    let rfc = need_rfc_id(&mailspace, &need);
    let (first, _) = foreign_prefixes(&mailspace);
    let pump = format!("pumper@{first}.local");

    // The done is the pump role's only letter: it is not its own earlier post.
    let tally = deliver(&mailspace, &pump, &need, &done_headers(&rfc), 1);
    assert_eq!(tally.applied, 0, "no earlier non-event post");
    assert_eq!(mailspace.item_event_state(&need).unwrap(), None);
}
