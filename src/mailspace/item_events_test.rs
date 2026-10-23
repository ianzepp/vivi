use super::property::{PropertyKind, carries_verdict, parse_property_event, verdict_report_item};
use super::*;
use crate::mailspace::SendRequest;

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    for role in ["mind", "hand", "auditor"] {
        mailspace.add_identity(role).unwrap();
    }
    (tmp, mailspace)
}

/// `(the delivered copy's handle, the sender's copy's handle)`.
fn send(mailspace: &Mailspace, kind: &str, to: &str) -> (String, String) {
    let result = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![to.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: format!("a {kind}"),
            body: crate::letter::fixture::body_for(kind, ""),
            role: format!("{kind}s"),
            kind: Some(kind.into()),
            reply_to: None,
        })
        .unwrap();
    let delivered = result
        .delivered
        .first()
        .map_or_else(|| result.sent.clone(), |d| d.handle.clone());
    (delivered, result.sent)
}

fn event(kind: ItemEventKind, id: &str, prev: &[&str], date: i64) -> EventLetter {
    EventLetter {
        kind,
        id: id.into(),
        of: "item".into(),
        prev: prev.iter().map(ToString::to_string).collect(),
        from_domain: "a.local".into(),
        at: date * 1_000_000,
    }
}

fn letter(headers: &str) -> Vec<u8> {
    format!(
        "From: mind@aaaaaaaa.local\r\nTo: hand@aaaaaaaa.local\r\nSubject: Re: x\r\n\
         Message-ID: <ev-1@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n{headers}\r\nbody\r\n"
    )
    .into_bytes()
}

#[test]
fn a_property_event_letter_carries_the_payload_its_kind_needs() {
    let marked = letter(
        "In-Reply-To: <item-1@x.test>\r\nX-Vivi-Event: marked\r\n\
         X-Vivi-Event-Of: <Item-1@x.test>\r\nX-Vivi-Event-Slug: ready\r\n",
    );
    let marked = parse_property_event(&marked).unwrap();
    assert_eq!(marked.kind, PropertyKind::Marked);
    assert_eq!(marked.of, "item-1@x.test");
    assert_eq!(marked.id, "ev-1@x.test");
    assert_eq!(marked.slug.as_deref(), Some("ready"));
    assert_eq!(marked.from_domain, "aaaaaaaa.local");

    let bound = letter(
        "In-Reply-To: <item@x.test>\r\nX-Vivi-Event: bound\r\nX-Vivi-Event-Of: <item@x.test>\r\n\
         X-Vivi-Event-Unit: <u1@x.test>\r\nX-Vivi-Event-Unit: <U1@x.test>\r\n",
    );
    let bound = parse_property_event(&bound).unwrap();
    assert_eq!(bound.units, ["u1@x.test"], "deduplicated");

    let sealed = letter(
        "In-Reply-To: <item@x.test>\r\nX-Vivi-Event: absorbed\r\nX-Vivi-Event-Of: <item@x.test>\r\n\
         X-Vivi-Event-State: Done\r\nX-Vivi-Event-At: 2026-10-14T02:21:03.123456Z\r\n",
    );
    let sealed = parse_property_event(&sealed).unwrap();
    assert_eq!(sealed.kind, PropertyKind::Absorbed);
    assert_eq!(sealed.state.as_deref(), Some("done"));
    assert_eq!(sealed.at, 1_791_944_463_123_456);
    assert!(parse_event(&letter(
        "In-Reply-To: <item@x.test>\r\nX-Vivi-Event: absorbed\r\nX-Vivi-Event-Of: <item@x.test>\r\n\
         X-Vivi-Event-State: done\r\n",
    ))
    .is_none(), "a state event ignores the property kinds");
}

#[test]
fn a_property_event_without_its_payload_or_a_matching_reply_is_plain_mail() {
    let no_payload = [
        "In-Reply-To: <item@x.test>\r\nX-Vivi-Event: absorbed\r\nX-Vivi-Event-Of: <item@x.test>\r\n",
        "In-Reply-To: <item@x.test>\r\nX-Vivi-Event: absorbed\r\nX-Vivi-Event-Of: <item@x.test>\r\n\
         X-Vivi-Event-State: cancelled\r\n",
        "In-Reply-To: <item@x.test>\r\nX-Vivi-Event: marked\r\nX-Vivi-Event-Of: <item@x.test>\r\n",
        "In-Reply-To: <item@x.test>\r\nX-Vivi-Event: bound\r\nX-Vivi-Event-Of: <item@x.test>\r\n",
        "In-Reply-To: <other@x.test>\r\nX-Vivi-Event: absorbed\r\nX-Vivi-Event-Of: <item@x.test>\r\n\
         X-Vivi-Event-State: done\r\n",
    ];
    for headers in no_payload {
        assert!(
            parse_property_event(&letter(headers)).is_none(),
            "{headers}"
        );
    }
}

#[test]
fn a_letter_carries_verdict_only_with_the_verdict_headers() {
    assert!(carries_verdict(&letter("X-Vivi-Verdict: fixed\r\n")));
    assert!(carries_verdict(&letter("X-Vivi-Repo: vivi=sha9\r\n")));
    assert!(carries_verdict(&letter(
        "X-Vivi-Branch: mind/review\r\nX-Vivi-Commit: deadbeef\r\n"
    )));
    assert!(carries_verdict(&letter(
        "X-Vivi-Branch: -\r\nX-Vivi-Commit: -\r\n"
    )));
    assert!(!carries_verdict(&letter("X-Vivi-Branch: mind/review\r\n")));
    assert!(
        !carries_verdict(&letter("X-Vivi-Repo: no-tip\r\n")),
        "a repo header without '=' carries nothing"
    );
    assert!(!carries_verdict(&letter("X-Vivi-Note: hi\r\n")));
}

#[test]
fn a_verdict_report_letter_names_the_live_item_it_replies_to() {
    let (_tmp, mailspace) = space();
    let (task, _) = send(&mailspace, "task", "hand");
    let storage = mailspace.storage().unwrap();
    let row = storage
        .message_by_id(&storage.resolve_message_token(&task).unwrap())
        .unwrap()
        .unwrap();
    // The row's `message_id` is a local handle; its RFC `Message-ID` is the
    // normalized one the reply letter names.
    let rfc_id = row
        .normalized_message_id
        .clone()
        .expect("a held Message-ID");
    let report = |headers: String| {
        format!(
            "From: hand@aaaaaaaa.local\r\nTo: mind@aaaaaaaa.local\r\nSubject: Re: x\r\n\
             Message-ID: <rep-1@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n{headers}\r\n\r\nbody\r\n"
        )
        .into_bytes()
    };
    let held = verdict_report_item(
        &storage,
        &report(format!(
            "In-Reply-To: <{}>\r\nX-Vivi-Verdict: fixed\r\n",
            rfc_id
        )),
    )
    .unwrap();
    assert_eq!(held.as_deref(), Some(row.content_id.as_str()));
    // Replies to a held row but carries no verdict headers.
    assert_eq!(
        verdict_report_item(&storage, &report(format!("In-Reply-To: <{rfc_id}>\r\n"))).unwrap(),
        None
    );
    // Carries verdict headers but replies to a row nothing here holds.
    assert_eq!(
        verdict_report_item(
            &storage,
            &report("In-Reply-To: <nope@x.test>\r\nX-Vivi-Verdict: fixed\r\n".into()),
        )
        .unwrap(),
        None
    );
}

/// The event letters held for `item`, oldest first, as `(kind, prev count)`.
fn events_of(mailspace: &Mailspace, handle: &str) -> Vec<(String, usize)> {
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(handle).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    let mut found: Vec<(i64, String, usize)> = Vec::new();
    for child in storage.reply_child_contents(&view.content_id).unwrap() {
        let row = storage.message_by_content_id(&child).unwrap().unwrap();
        let bytes = storage.read_listed_blob(&row.blob_relpath).unwrap();
        if let Some(event) = parse_event(&bytes) {
            found.push((event.at, event.kind.header().into(), event.prev.len()));
        }
    }
    found.sort();
    found
        .into_iter()
        .map(|(_, kind, prev)| (kind, prev))
        .collect()
}

#[test]
fn an_event_letter_names_its_item_twice_and_a_kind() {
    let bytes = letter(
        "In-Reply-To: <item-1@x.test>\r\nX-Vivi-Event: done\r\n\
         X-Vivi-Event-Of: <Item-1@x.test>\r\nX-Vivi-Event-Prev: <p1@x.test>\r\n\
         X-Vivi-Event-Prev: <p2@x.test>\r\nX-Vivi-Event-Prev: <p1@x.test>\r\n",
    );

    let parsed = parse_event(&bytes).unwrap();

    assert_eq!(parsed.kind, ItemEventKind::Done);
    assert_eq!(parsed.of, "item-1@x.test");
    assert_eq!(parsed.id, "ev-1@x.test");
    assert_eq!(parsed.prev, ["p1@x.test", "p2@x.test"], "deduplicated");
    assert_eq!(parsed.from_domain, "aaaaaaaa.local");
    assert_eq!(parsed.at, 1_791_720_000 * 1_000_000, "the Date header");
}

#[test]
fn a_letter_that_does_not_reply_to_its_item_or_has_no_kind_is_plain_mail() {
    let elsewhere = letter(
        "In-Reply-To: <other@x.test>\r\nX-Vivi-Event: done\r\nX-Vivi-Event-Of: <item@x.test>\r\n",
    );
    let unknown = letter(
        "In-Reply-To: <item@x.test>\r\nX-Vivi-Event: cancelled\r\nX-Vivi-Event-Of: <item@x.test>\r\n",
    );
    let bare = letter("In-Reply-To: <item@x.test>\r\n");

    assert!(parse_event(&elsewhere).is_none());
    assert!(parse_event(&unknown).is_none());
    assert!(parse_event(&bare).is_none());
}

#[test]
fn the_head_is_the_event_no_other_names_whatever_the_dates() {
    let events = [
        event(ItemEventKind::Done, "d", &[], 200),
        event(ItemEventKind::Reopened, "r", &["d"], 100),
    ];

    let heads = chain_heads(&events);

    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0].id, "r");
}

#[test]
fn a_fork_has_every_unnamed_event_as_a_head_and_the_later_date_then_id_wins() {
    let events = [
        event(ItemEventKind::Done, "a", &[], 100),
        event(ItemEventKind::Reopened, "b", &[], 100),
        event(ItemEventKind::Done, "c", &["a"], 90),
    ];

    let heads = chain_heads(&events);

    let ids: Vec<&str> = heads.iter().map(|head| head.id.as_str()).collect();
    assert_eq!(ids, ["c", "b"], "oldest to newest; the last is current");
}

#[test]
fn a_cycle_has_no_head_so_every_event_is_a_candidate() {
    let events = [
        event(ItemEventKind::Done, "a", &["b"], 100),
        event(ItemEventKind::Reopened, "b", &["a"], 200),
    ];

    let heads = chain_heads(&events);

    assert_eq!(heads.last().unwrap().id, "b");
}

#[test]
fn the_newest_event_is_decided_by_the_clock_then_the_chain_then_the_id() {
    // A later clock beats the chain: the successor loses to a later `at`.
    let later = event(ItemEventKind::Done, "d", &[], 200);
    let skewed = event(ItemEventKind::Reopened, "r", &["d"], 100);
    assert_eq!(
        newest_event(&[skewed.clone(), later.clone()]).unwrap().id,
        "d"
    );

    // Equal clock: the successor wins even over a larger Message-ID.
    let larger = event(ItemEventKind::Done, "z", &[], 100);
    let successor = event(ItemEventKind::Reopened, "r", &["z"], 100);
    assert_eq!(newest_event(&[larger, successor.clone()]).unwrap().id, "r");

    // Equal clock and no Prev between two: the larger Message-ID wins.
    let small = event(ItemEventKind::Reopened, "a", &[], 100);
    let fork = event(ItemEventKind::Done, "y", &[], 100);
    assert_eq!(newest_event(&[small, fork]).unwrap().id, "y");

    // A stamp from `X-Vivi-Event-At` is an `at` like any other.
    let stamped = EventLetter {
        at: 201 * 1_000_000,
        ..later
    };
    assert_eq!(newest_event(&[successor, stamped]).unwrap().id, "d");
}

#[test]
fn an_event_at_stamp_beats_the_date_header() {
    let stamped = letter(
        "In-Reply-To: <item@x.test>\r\nX-Vivi-Event: done\r\nX-Vivi-Event-Of: <item@x.test>\r\n\
         X-Vivi-Event-At: 2026-10-14T02:21:03.123456Z\r\n",
    );
    let unparsable = letter(
        "In-Reply-To: <item@x.test>\r\nX-Vivi-Event: done\r\nX-Vivi-Event-Of: <item@x.test>\r\n\
         X-Vivi-Event-At: sometime\r\n",
    );

    let parsed = parse_event(&stamped).unwrap();
    assert_eq!(parsed.at, 1_791_944_463_123_456);
    let fallback = parse_event(&unparsable).unwrap();
    assert_eq!(fallback.at, 1_791_720_000 * 1_000_000, "falls back to Date");
}

#[test]
fn the_owners_close_and_reopen_are_chained_reply_letters() {
    let (_tmp, mailspace) = space();
    let (task, sent) = send(&mailspace, "task", "hand");

    mailspace
        .move_task("mind", &sent, "done", Some("accepted"), None, &[], &[])
        .unwrap();
    let first = mailspace.item_event_state(&task).unwrap().unwrap();
    mailspace
        .move_task("mind", &sent, "tasks", None, None, &[], &[])
        .unwrap();
    let second = mailspace.item_event_state(&task).unwrap().unwrap();

    assert_eq!(first.state, "done");
    assert_eq!(second.state, "reopened");
    assert_eq!(second.heads, 1, "the reopen names the close");
    assert_ne!(first.event, second.event);
    assert_eq!(
        events_of(&mailspace, &task),
        [("done".to_string(), 0), ("reopened".to_string(), 1)]
    );
    // The assignee's copy followed both, and the event letters are read.
    assert_eq!(
        mailspace.list("hand", "tasks").unwrap().len(),
        1,
        "reopened"
    );
    let inbox = mailspace.list("hand", "inbox").unwrap();
    assert_eq!(inbox.len(), 2);
    assert!(inbox.iter().all(|row| row.read_state));
    assert!(inbox.iter().all(|row| row.subject == "Re: a task"));
}

#[test]
fn the_event_letter_goes_to_the_items_recipients_minus_the_sender() {
    let (_tmp, mailspace) = space();
    let result = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into(), "mind".into()],
            cc: vec!["auditor".into(), "hand@3f9a1c7e.local".into()],
            bcc: Vec::new(),
            subject: "wide".into(),
            body: crate::letter::fixture::need_body(&["ok"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap();

    mailspace
        .move_item("mind", &result.sent, "done", None, "need done", None)
        .unwrap();

    let storage = mailspace.storage().unwrap();
    let event_row = storage
        .list_messages_by_role("sent")
        .unwrap()
        .into_iter()
        .find(|row| row.subject == "Re: wide")
        .unwrap();
    assert!(event_row.from_addr.starts_with("mind@"));
    assert!(event_row.to_addr.contains("hand@"), "{}", event_row.to_addr);
    assert!(
        !event_row.to_addr.contains("mind@"),
        "{}",
        event_row.to_addr
    );
    assert!(
        event_row.cc_addr.contains("auditor@"),
        "{}",
        event_row.cc_addr
    );
    assert!(
        event_row.cc_addr.contains("hand@3f9a1c7e.local"),
        "{}",
        event_row.cc_addr
    );
    // Delivered read to the local roles; the remote Cc is reached by sync.
    for role in ["hand", "auditor"] {
        let rows = mailspace.list(role, "inbox").unwrap();
        assert_eq!(rows.len(), 1, "{role}");
        assert!(rows[0].read_state);
    }
}

#[test]
fn an_assignee_may_not_reopen_what_its_owner_closed() {
    let (_tmp, mailspace) = space();
    let (task, sent) = send(&mailspace, "task", "hand");
    mailspace
        .move_task("mind", &sent, "done", None, None, &[], &[])
        .unwrap();

    let error = mailspace
        .move_task("hand", &task, "tasks", None, None, &[], &[])
        .unwrap_err();

    assert!(
        error.to_string().contains("only the owner can reopen it"),
        "{error}"
    );
    assert_eq!(mailspace.list("hand", "done").unwrap().len(), 1);
}

#[test]
fn an_assignees_close_is_a_local_report_without_an_event() {
    let (_tmp, mailspace) = space();
    let (task, _) = send(&mailspace, "task", "hand");

    mailspace
        .move_task("hand", &task, "done", Some("finished"), None, &[], &[])
        .unwrap();

    assert_eq!(mailspace.item_event_state(&task).unwrap(), None);
    assert_eq!(mailspace.list("hand", "done").unwrap().len(), 1);
    assert!(events_of(&mailspace, &task).is_empty());
}

#[test]
fn wants_close_and_reopen_by_the_same_letters() {
    let (_tmp, mailspace) = space();
    let (want, sent) = send(&mailspace, "want", "hand");

    mailspace
        .move_item("mind", &sent, "done", None, "want drop", None)
        .unwrap();
    assert_eq!(mailspace.list("hand", "done").unwrap().len(), 1);
    assert_eq!(
        mailspace.item_event_state(&want).unwrap().unwrap().state,
        "done"
    );

    mailspace
        .move_item("mind", &sent, "wants", None, "item move", None)
        .unwrap();
    assert_eq!(mailspace.list("hand", "wants").unwrap().len(), 1);
    assert_eq!(
        mailspace.item_event_state(&want).unwrap().unwrap().state,
        "reopened"
    );
}

#[test]
fn applying_the_same_head_again_changes_nothing() {
    let (_tmp, mailspace) = space();
    let (task, sent) = send(&mailspace, "task", "hand");
    mailspace
        .move_task("mind", &sent, "done", None, None, &[], &[])
        .unwrap();
    let content = {
        let storage = mailspace.storage().unwrap();
        let id = storage.resolve_message_token(&task).unwrap();
        storage.message_by_id(&id).unwrap().unwrap().content_id
    };
    let before = mailspace.storage().unwrap().list_messages().unwrap();

    let tally = mailspace
        .apply_arrived_events(std::slice::from_ref(&content))
        .unwrap();

    assert_eq!(tally, EventTally::default());
    assert_eq!(
        mailspace.storage().unwrap().list_messages().unwrap(),
        before
    );
}

#[test]
fn release_due_reads_only_the_named_item() {
    let (_tmp, mailspace) = space();
    let (a, _) = send(&mailspace, "need", "hand");
    let (b, _) = send(&mailspace, "need", "hand");
    let storage = mailspace.storage().unwrap();
    let (row_a_id, content_a) = {
        let id = storage.resolve_message_token(&a).unwrap();
        let row = storage.message_by_id(&id).unwrap().unwrap();
        (row.message_id, row.content_id)
    };
    let content_b = {
        let id = storage.resolve_message_token(&b).unwrap();
        storage.message_by_id(&id).unwrap().unwrap().content_id
    };

    storage
        .set_event_deferral(&[row_a_id], Some(clock_micros() + 60_000_000))
        .unwrap();
    assert!(!event_release_due(&storage, &content_a).unwrap());
    assert!(!event_release_due(&storage, &content_b).unwrap());

    let (due_a, due_b) = with_clock_skew(61_000_000, || {
        (
            event_release_due(&storage, &content_a).unwrap(),
            event_release_due(&storage, &content_b).unwrap(),
        )
    });
    assert!(due_a, "the marked item's bound has passed");
    assert!(!due_b, "the other item holds no deferral of its own");
}

#[test]
fn failed_release_keeps_the_deferral_mark() {
    let (_tmp, mailspace) = space();
    let (delivered, _sent) = send(&mailspace, "need", "hand");
    let storage = mailspace.storage().unwrap();
    let row_id = storage.resolve_message_token(&delivered).unwrap();
    let content_id = storage
        .message_by_id(&row_id)
        .unwrap()
        .unwrap()
        .content_id
        .clone();
    // A mark whose bound the clock has passed: the release is due.
    let bound = clock_micros() - 1;
    storage.set_event_deferral(&[row_id], Some(bound)).unwrap();

    arm_fail_next_settle();
    assert!(
        mailspace.apply_released_events().is_err(),
        "the armed settle fails"
    );
    assert!(
        storage
            .deferred_event_contents()
            .unwrap()
            .contains(&(content_id, bound)),
        "a settle that errors must leave the mark, so the next pass retries"
    );

    assert!(mailspace.apply_released_events().is_ok());
    assert!(
        storage.deferred_event_contents().unwrap().is_empty(),
        "a successful settle drops the mark"
    );
}

/// The raw bytes of the event letters held for `handle`, oldest first.
fn event_letters(mailspace: &Mailspace, handle: &str) -> Vec<String> {
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(handle).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    let mut found: Vec<(i64, String)> = Vec::new();
    for child in storage.reply_child_contents(&view.content_id).unwrap() {
        let row = storage.message_by_content_id(&child).unwrap().unwrap();
        let bytes = storage.read_listed_blob(&row.blob_relpath).unwrap();
        if let Some(event) = parse_event(&bytes) {
            found.push((event.at, String::from_utf8_lossy(&bytes).into_owned()));
        }
    }
    found.sort();
    found.into_iter().map(|(_, bytes)| bytes).collect()
}

#[test]
fn want_promote_writes_a_promoted_event_letter() {
    let (_tmp, mailspace) = space();
    // A want of mind's own: mind holds the delivered copy in `wants`, so the
    // promotion is the author's move and travels.
    let (want, _sent) = send(&mailspace, "want", "mind");

    let promoted = mailspace
        .move_item("mind", &want, "needs", None, "want promote", None)
        .unwrap();

    assert!(
        mailspace
            .list("mind", "needs")
            .unwrap()
            .iter()
            .any(|row| row.handle == promoted),
        "the promoted row is in needs"
    );
    let state = mailspace.item_event_state(&want).unwrap().unwrap();
    assert_eq!(state.state, "promoted");
    assert_eq!(events_of(&mailspace, &want), [("promoted".to_string(), 0)]);
    let letters = event_letters(&mailspace, &promoted);
    assert_eq!(letters.len(), 1);
    assert!(
        letters[0].contains("X-Vivi-Event: promoted"),
        "{}",
        letters[0]
    );
    // A promotion is not a close: the backlog node stays open.
    let node = mailspace
        .graph_show("backlog")
        .unwrap()
        .nodes
        .into_iter()
        .find(|node| node.source_id == promoted)
        .expect("the promoted want has a backlog node");
    assert_eq!(node.state, "open");
}

#[test]
fn a_hand_promoting_another_roles_want_stays_a_local_move() {
    let (_tmp, mailspace) = space();
    let (want, _sent) = send(&mailspace, "want", "hand");

    let moved = mailspace
        .move_item("hand", &want, "needs", None, "want promote", None)
        .unwrap();

    assert_eq!(mailspace.item_event_state(&want).unwrap(), None);
    assert!(events_of(&mailspace, &want).is_empty());
    assert!(
        mailspace
            .list("hand", "needs")
            .unwrap()
            .iter()
            .any(|row| row.handle == moved),
        "the reporter's copy still moves"
    );
}
