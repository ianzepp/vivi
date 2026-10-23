//! The event order is a total order, and a far-future instant cannot lock an
//! item: three equal-`at` letters whose pairwise comparisons cycle still
//! have one winner on every ordering, and a letter dated past the clock is
//! skipped the way an unauthorized one is, so `done`, `reopened`, `absorbed`,
//! `marked` and `bound` cannot arrive from the future.

use super::emit;
use super::*;
use crate::mailspace::SendRequest;
use crate::storage::MessageIngestRequest;

/// The last instant an RFC 3339 stamp can say with four year digits.
const YEAR_9999: &str = "9999-12-31T23:59:59.999999Z";
/// The `Date` of the same instant, for a letter that carries no stamp.
const DATE_9999: &str = "Fri, 31 Dec 9999 23:59:59 +0000";
/// Ten minutes in microseconds: the clock skew a counted event may carry.
const TEN_MINUTES: i64 = 10 * 60 * 1_000_000;

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    for role in ["mind", "hand"] {
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
            body: "done_when: ok\n".into(),
            role: format!("{kind}s"),
            kind: Some(kind.into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let delivered = result
        .delivered
        .first()
        .map_or_else(|| result.sent.clone(), |delivered| delivered.handle.clone());
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

/// Store one raw reply letter beside `item`, from a member's prefix domain
/// the way a pull files a letter, and return its content id.
fn put_letter(mailspace: &Mailspace, item: &str, id: &str, date: &str, extra: &str) -> String {
    let storage = mailspace.storage().unwrap();
    let message_id = storage.resolve_message_token(item).unwrap();
    let view = storage.message_by_id(&message_id).unwrap().unwrap();
    let item_held = held_item(&storage, &view.content_id).unwrap().unwrap();
    let bytes = format!(
        "From: hand@3f9a1c7e.local\r\nTo: mind@3f9a1c7e.local\r\nSubject: Re: a task\r\n\
         Message-ID: <{id}>\r\nDate: {date}\r\nIn-Reply-To: <{rfc}>\r\n\
         X-Vivi-Event-Of: <{rfc}>\r\n{extra}\r\nbody\r\n",
        rfc = item_held.rfc_id
    )
    .into_bytes();
    let written = storage
        .apply_item_event(&ItemEventWrite {
            reply_requests: &[MessageIngestRequest {
                account: "hand".into(),
                local_role: "inbox".into(),
                read_state: true,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("order-test\0{id}"),
                remote: None,
            }],
            reply_data: Some(&bytes),
            parent_content_id: &view.content_id,
            moves: &[],
        })
        .unwrap();
    written[0].content_id.clone()
}

/// The state-event letters held for `item`, with the `X-Vivi-Event-At`
/// header text each carries (empty when it carries none).
fn held_events(mailspace: &Mailspace, item: &str) -> Vec<(EventLetter, String)> {
    let storage = mailspace.storage().unwrap();
    let message_id = storage.resolve_message_token(item).unwrap();
    let view = storage.message_by_id(&message_id).unwrap().unwrap();
    let mut found = Vec::new();
    for child in storage.reply_child_contents(&view.content_id).unwrap() {
        let row = storage.message_by_content_id(&child).unwrap().unwrap();
        let bytes = storage.read_listed_blob(&row.blob_relpath).unwrap();
        let Some(parsed) = parse_event(&bytes) else {
            continue;
        };
        let stamp = mail_parser::MessageParser::default()
            .parse(&bytes)
            .unwrap()
            .headers()
            .iter()
            .find(|header| header.name().eq_ignore_ascii_case("X-Vivi-Event-At"))
            .and_then(|header| match header.value() {
                mail_parser::HeaderValue::Text(text) => Some(text.trim().to_string()),
                _ => None,
            })
            .unwrap_or_default();
        found.push((parsed, stamp));
    }
    found
}

/// The folded marks of the item's own row, the way `task show` prints them.
fn marks_of(mailspace: &Mailspace, item: &str) -> Vec<String> {
    let storage = mailspace.storage().unwrap();
    let message_id = storage.resolve_message_token(item).unwrap();
    mailspace
        .thread(item, false, 50, 50)
        .unwrap()
        .into_iter()
        .find(|message| message.message_id == message_id)
        .unwrap()
        .marks
}

#[test]
fn the_newest_event_is_the_same_for_every_ordering_of_the_same_letters() {
    // Three equal-`at` letters whose pairwise comparisons cycle: B names A
    // (so it beats A), C's id beats B, and A's id beats C. Only a total
    // order can pick the same winner for every ordering of the slice.
    let a = event(ItemEventKind::Done, "m", &[], 100);
    let b = event(ItemEventKind::Reopened, "a", &["m"], 100);
    let c = event(ItemEventKind::Done, "b", &[], 100);
    let held = [&a, &b, &c];
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let events: Vec<EventLetter> = order.iter().map(|&index| held[index].clone()).collect();
        assert_eq!(newest_event(&events).unwrap().id, "a", "{order:?}");
    }
}

#[test]
fn a_state_event_dated_past_the_clock_is_ignored_and_locks_nothing() {
    let (_tmp, mailspace) = space();
    let (task, sent) = send(&mailspace, "task", "hand");
    mailspace
        .move_task("mind", &sent, "done", None, None, &[], &[])
        .unwrap();
    mailspace
        .move_task("mind", &sent, "tasks", None, None, &[], &[])
        .unwrap();

    let future = put_letter(
        &mailspace,
        &task,
        "future-done@x.test",
        "Sun, 11 Oct 2026 12:00:00 +0000",
        &format!("X-Vivi-Event: done\r\nX-Vivi-Event-At: {YEAR_9999}\r\n"),
    );
    let tally = mailspace.apply_arrived_events(&[future]).unwrap();

    assert_eq!(tally.applied, 0);
    assert_eq!(tally.ignored, 1, "a future instant counts ignored");
    assert_eq!(
        mailspace.item_event_state(&task).unwrap().unwrap().state,
        "reopened",
        "the previous event still says the state"
    );
    assert_eq!(
        mailspace.list("hand", "tasks").unwrap().len(),
        1,
        "no row moved to done"
    );
    // A later honest close from a member still lands, stamped with a normal
    // instant: the ignored letter is not the writer's floor either.
    mailspace
        .move_task("mind", &sent, "done", None, None, &[], &[])
        .unwrap();
    assert_eq!(
        mailspace.item_event_state(&task).unwrap().unwrap().state,
        "done"
    );
    let now = chrono::Utc::now().timestamp_micros();
    let held = held_events(&mailspace, &task);
    let (future, stamp) = held
        .iter()
        .find(|(letter, _)| letter.id == "future-done@x.test")
        .unwrap();
    assert_eq!(
        rfc3339_micros(stamp).unwrap(),
        rfc3339_micros(YEAR_9999).unwrap()
    );
    assert!(future.at > now, "the letter is dated past the clock");
    let (_, close_stamp) = held
        .iter()
        .find(|(letter, _)| letter.kind == ItemEventKind::Done && letter.id != "future-done@x.test")
        .unwrap();
    let at = rfc3339_micros(close_stamp).expect("the stamp parses back");
    assert!((at - now).abs() <= TEN_MINUTES, "{close_stamp} vs {now}");
}

#[test]
fn a_date_only_event_dated_past_the_clock_is_ignored_too() {
    let (_tmp, mailspace) = space();
    let (task, sent) = send(&mailspace, "task", "hand");
    mailspace
        .move_task("mind", &sent, "done", None, None, &[], &[])
        .unwrap();

    let future = put_letter(
        &mailspace,
        &task,
        "future-reopen@x.test",
        DATE_9999,
        "X-Vivi-Event: reopened\r\n",
    );
    let tally = mailspace.apply_arrived_events(&[future]).unwrap();

    assert_eq!(tally.applied, 0);
    assert_eq!(tally.ignored, 1);
    assert_eq!(
        mailspace.item_event_state(&task).unwrap().unwrap().state,
        "done",
        "the Date header is the instant, and it is past the clock"
    );
    assert_eq!(mailspace.list("hand", "done").unwrap().len(), 1);
}

#[test]
fn a_marked_letter_dated_past_the_clock_changes_no_marks() {
    let (_tmp, mailspace) = space();
    let (task, _) = send(&mailspace, "task", "hand");
    let now = put_letter(
        &mailspace,
        &task,
        "marked-now@x.test",
        "Sun, 11 Oct 2026 12:00:00 +0000",
        "X-Vivi-Event: marked\r\nX-Vivi-Event-Slug: ready\r\n",
    );
    mailspace.apply_arrived_events(&[now]).unwrap();
    assert_eq!(marks_of(&mailspace, &task), vec!["ready".to_string()]);

    let future = put_letter(
        &mailspace,
        &task,
        "unmarked-future@x.test",
        "Sun, 11 Oct 2026 12:00:00 +0000",
        &format!(
            "X-Vivi-Event: unmarked\r\nX-Vivi-Event-Slug: ready\r\n\
             X-Vivi-Event-At: {YEAR_9999}\r\n"
        ),
    );
    let tally = mailspace.apply_arrived_events(&[future]).unwrap();

    assert_eq!(tally.applied, 0);
    assert_eq!(tally.ignored, 1);
    assert_eq!(
        marks_of(&mailspace, &task),
        vec!["ready".to_string()],
        "a letter from the future moves no mark"
    );
}

#[test]
fn a_stamp_past_the_last_four_digit_year_falls_back_to_the_clock() {
    let end_of_time = rfc3339_micros(YEAR_9999).unwrap() + 1;

    let stamp = emit::stamp_micros(end_of_time);

    let now = chrono::Utc::now().timestamp_micros();
    assert!(!stamp.starts_with('+'), "{stamp}");
    let at = rfc3339_micros(&stamp).expect("the stamp parses back");
    assert!((at - now).abs() <= TEN_MINUTES, "{stamp}");
}
