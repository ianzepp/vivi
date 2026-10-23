//! `send = "addressed"`: a `LocalOnly` event letter is re-evaluated on every
//! push.
//!
//! An event letter for an item that is not yet on the server is skipped as
//! [`SkipReason::LocalOnly`]. That skip is this push's verdict, not the
//! letter's fate: once the item itself is bound to the project (pushed or
//! pulled alone), the event follows work that has left the machine and the
//! next push under `send = "addressed"` selects it. These tests prove it for
//! every event kind, including letters written before their item was ever
//! pushed.

use std::rc::Rc;

use crate::letter::fixture::body_for;
use crate::mailspace::{AbsorbConfig, SendRequest};
use crate::sync::fake_server::{Checkout, FakeServer, PROJECT, checkout};
use crate::sync::push;
use crate::sync::select::{Selection, SkipReason};
use crate::sync::{SyncSettings, sync_dir};

/// Flip `[sync]` `send` to `value` and re-resolve the settings.
fn set_send(co: &mut Checkout, value: Option<&str>) {
    co.mailspace.config.sync.as_mut().unwrap().send = value.map(str::to_string);
    co.settings = SyncSettings::resolve(&co.mailspace).unwrap();
}

/// A checkout whose `[sync]` `send` key is `addressed`.
fn addressed(name: &str, roles: &[&str]) -> Checkout {
    let mut co = checkout(name, roles);
    set_send(&mut co, Some("addressed"));
    co
}

/// Send an item the way the commands do, addressed to this machine's own
/// `hand` only, so under `send = "addressed"` it starts `LocalOnly`.
fn send_item_to_own_hand(co: &Checkout, role: &str, kind: &str, subject: &str) -> String {
    let result = co
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![co.address("hand")],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: body_for(kind, &format!("{subject} body\n")),
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

fn pending(co: &Checkout) -> Selection {
    push::pending(&co.mailspace, &co.settings).unwrap()
}

/// The handle of the sender's own (`sent`) copy of `item`: the row its
/// author holds, which `absorb` and `set_marks` resolve strictly.
fn sent_copy_handle(co: &Checkout, item: &str) -> String {
    let content = co.row(&co.id_of(item)).unwrap().content_id;
    co.rows()
        .into_iter()
        .find(|row| row.content_id == content && row.local_role == "sent")
        .unwrap()
        .handle
}

fn assert_all_local_only(selection: &Selection) {
    assert!(
        selection
            .skipped
            .iter()
            .all(|skipped| skipped.reason == SkipReason::LocalOnly),
        "{selection:?}"
    );
}

/// Every skipped handle except `item`'s own letter.
fn handles_except(selection: &Selection, item: &str) -> Vec<String> {
    selection
        .skipped
        .iter()
        .map(|skipped| skipped.handle.clone())
        .filter(|handle| handle.as_str() != item)
        .collect()
}

/// The one skipped handle that is not `item`'s own letter: the event letter
/// on it.
fn the_event_letter(selection: &Selection, item: &str) -> String {
    let others = handles_except(selection, item);
    assert_eq!(others.len(), 1, "{selection:?}");
    others.into_iter().next().unwrap()
}

/// The value of a letter's `X-Vivi-Event` header, when it has one.
fn event_of(co: &Checkout, handle: &str) -> Option<String> {
    let bytes = co.bytes_of(&co.id_of(handle));
    let parsed = mail_parser::MessageParser::default().parse(&bytes)?;
    parsed
        .headers()
        .iter()
        .find(|header| header.name().eq_ignore_ascii_case("X-Vivi-Event"))
        .and_then(|header| match header.value() {
            mail_parser::HeaderValue::Text(text) => Some(text.trim().to_string()),
            _ => None,
        })
}

/// The one handle in `handles` whose letter is an `event` letter.
fn the_letter_of_event(co: &Checkout, handles: &[String], event: &str) -> String {
    let matched: Vec<String> = handles
        .iter()
        .filter(|handle| event_of(co, handle).as_deref() == Some(event))
        .cloned()
        .collect();
    assert_eq!(matched.len(), 1, "{handles:?}");
    matched.into_iter().next().unwrap()
}

/// Record `handles` in the kept-local list push reads — without the stuck
/// check the command applies — so a push tries none of them until
/// [`undo_keep`] lifts the record.
fn keep_for_push(co: &Checkout, handles: &[String]) {
    let ids: Vec<String> = handles
        .iter()
        .map(|handle| co.row(&co.id_of(handle)).unwrap().content_id)
        .collect();
    let dir = sync_dir(&co.mailspace.dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("kept-local.json"),
        serde_json::to_string(&ids).unwrap(),
    )
    .unwrap();
}

/// Lift the kept-local record, as `sync keep-local --undo` does.
fn undo_keep(co: &Checkout) {
    std::fs::remove_file(sync_dir(&co.mailspace.dir).join("kept-local.json")).unwrap();
}

/// Make `item`'s row bound to the project without pushing the letters in
/// `kept`: record them kept local, push once under `send = "all"` (which
/// must send exactly `expected` letters), lift the record, and restore
/// `send = "addressed"`. This is the item pushed or pulled alone: its row
/// comes back carrying the project binding.
fn push_item_alone(
    co: &mut Checkout,
    server: &Rc<FakeServer>,
    item: &str,
    kept: &[String],
    expected: usize,
) {
    keep_for_push(co, kept);
    set_send(co, Some("all"));
    let report = co.push(&co.client(server));
    assert_eq!(report.sent, expected, "{report:?}");
    undo_keep(co);
    set_send(co, Some("addressed"));
    assert!(
        co.row(&co.id_of(item))
            .unwrap()
            .remote
            .as_ref()
            .is_some_and(|remote| remote.remote_mailbox == PROJECT),
        "the item is bound to the project"
    );
}

#[test]
fn a_local_only_done_letter_is_selected_once_its_need_is_bound() {
    let server = FakeServer::new();
    let mut a = addressed("alice", &["mind", "hand"]);
    let need = send_item_to_own_hand(&a, "needs", "need", "a local need");
    a.mailspace
        .move_task("mind", &need, "done", None, None, &[], &[])
        .unwrap();

    // Nothing is pushed: the need and its done letter are both local only.
    let selection = pending(&a);
    assert!(selection.letters.is_empty(), "{selection:?}");
    assert_eq!(selection.unsent(), 0);
    assert_eq!(selection.skipped.len(), 2, "{selection:?}");
    assert_all_local_only(&selection);
    let done = the_event_letter(&selection, &need);

    // The need alone becomes bound to the project; the done letter stays
    // unpushed.
    push_item_alone(&mut a, &server, &need, std::slice::from_ref(&done), 1);

    // The next push under `send = "addressed"` selects the done letter: the
    // event follows work that has left the machine.
    let selection = pending(&a);
    assert_eq!(selection.letters.len(), 1, "{selection:?}");
    assert_eq!(selection.letters[0].handle, done);
    assert_eq!(selection.letters[0].kind, "mail", "an event is plain mail");
    assert!(selection.skipped.is_empty(), "{selection:?}");
    assert_eq!(selection.unsent(), 1);
    assert_eq!(a.push(&a.client(&server)).sent, 1);
}

#[test]
fn local_only_reopened_letters_are_selected_once_their_need_is_bound() {
    let server = FakeServer::new();
    let mut a = addressed("bella", &["mind", "hand"]);
    let need = send_item_to_own_hand(&a, "needs", "need", "close and reopen me");
    a.mailspace
        .move_task("mind", &need, "done", None, None, &[], &[])
        .unwrap();
    a.mailspace
        .move_task("mind", &need, "needs", None, None, &[], &[])
        .unwrap();

    let selection = pending(&a);
    assert!(selection.letters.is_empty(), "{selection:?}");
    assert_eq!(selection.skipped.len(), 3, "{selection:?}");
    assert_all_local_only(&selection);
    let events = handles_except(&selection, &need);
    assert_eq!(events.len(), 2);

    // The need alone becomes bound; both event letters were written before
    // that and stay unpushed.
    let kept: Vec<String> = events.clone();
    push_item_alone(&mut a, &server, &need, &kept, 1);

    let selection = pending(&a);
    let handles: Vec<&str> = selection
        .letters
        .iter()
        .map(|letter| letter.handle.as_str())
        .collect();
    assert_eq!(handles.len(), 2, "{selection:?}");
    assert!(
        handles.contains(&events[0].as_str()) && handles.contains(&events[1].as_str()),
        "{handles:?}"
    );
    assert_eq!(a.push(&a.client(&server)).sent, 2);
}

#[test]
fn a_local_only_absorbed_letter_is_selected_once_its_need_is_bound() {
    let server = FakeServer::new();
    let mut a = addressed("cecil", &["mind", "hand"]);
    // The need is closed and absorbed in one breath here, so the operator
    // ruling's quiet wait is off; absorb_quiet_test covers the wait itself.
    a.mailspace.config.absorb = Some(AbsorbConfig { quiet_hours: 0 });
    let need = send_item_to_own_hand(&a, "needs", "need", "seal me");
    a.mailspace
        .move_task("mind", &need, "done", None, None, &[], &[])
        .unwrap();
    a.mailspace
        .absorb("mind", &sent_copy_handle(&a, &need), None, "need")
        .unwrap();

    let selection = pending(&a);
    assert!(selection.letters.is_empty(), "{selection:?}");
    assert_eq!(selection.skipped.len(), 3, "{selection:?}");
    assert_all_local_only(&selection);
    let events = handles_except(&selection, &need);
    assert_eq!(events.len(), 2, "the done and absorbed letters");
    let absorbed = the_letter_of_event(&a, &events, "absorbed");

    push_item_alone(&mut a, &server, &need, &events, 1);

    let selection = pending(&a);
    assert_eq!(selection.letters.len(), 2, "{selection:?}");
    let handles: Vec<&str> = selection
        .letters
        .iter()
        .map(|letter| letter.handle.as_str())
        .collect();
    assert!(handles.contains(&absorbed.as_str()), "{handles:?}");
    assert_eq!(a.push(&a.client(&server)).sent, 2);
}

#[test]
fn a_local_only_marked_letter_is_selected_once_its_need_is_bound() {
    let server = FakeServer::new();
    let mut a = addressed("dana", &["mind", "hand"]);
    let need = send_item_to_own_hand(&a, "needs", "need", "mark me");
    a.mailspace
        .set_marks(
            "mind",
            &sent_copy_handle(&a, &need),
            "need",
            true,
            &["ready".into()],
        )
        .unwrap();

    let selection = pending(&a);
    assert!(selection.letters.is_empty(), "{selection:?}");
    assert_eq!(selection.skipped.len(), 2, "{selection:?}");
    assert_all_local_only(&selection);
    let marked = the_event_letter(&selection, &need);

    push_item_alone(&mut a, &server, &need, std::slice::from_ref(&marked), 1);

    let selection = pending(&a);
    assert_eq!(selection.letters.len(), 1, "{selection:?}");
    assert_eq!(selection.letters[0].handle, marked);
    assert_eq!(a.push(&a.client(&server)).sent, 1);
}

#[test]
fn a_local_only_bound_letter_is_selected_once_its_need_is_bound() {
    let server = FakeServer::new();
    let mut a = addressed("evan", &["mind", "hand"]);
    let need = send_item_to_own_hand(&a, "needs", "need", "lower onto me");
    let unit = send_item_to_own_hand(&a, "tasks", "task", "the unit");
    a.mailspace
        .backlog_bind_units(&need, &[unit.clone()])
        .unwrap();

    // The need, the unit, and the bound letter on the need: all local only.
    let selection = pending(&a);
    assert!(selection.letters.is_empty(), "{selection:?}");
    assert_eq!(selection.skipped.len(), 3, "{selection:?}");
    assert_all_local_only(&selection);
    let bound = handles_except(&selection, &need)
        .into_iter()
        .find(|handle| handle.as_str() != unit)
        .unwrap();

    // The need and the unit alone become bound; the bound letter does not.
    push_item_alone(&mut a, &server, &need, &[bound.clone()], 2);

    let selection = pending(&a);
    assert_eq!(selection.letters.len(), 1, "{selection:?}");
    assert_eq!(selection.letters[0].handle, bound);
    assert_eq!(a.push(&a.client(&server)).sent, 1);
}
