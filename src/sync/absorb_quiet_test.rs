//! The operator ruling of 2026-10-14 across mailspaces: the quiet wait an
//! absorb by a closer honors (default 24 hours, `[absorb]` `quiet_hours`),
//! the state the author's absorb letter names for its own `sent` row of a
//! closed item, and the one seal two concurrent absorbs converge on.

use crate::letter::fixture::{need_body, task_body};
use crate::mailspace::{AbsorbConfig, Mailspace, RoleUpdate, SendRequest};
use crate::storage::StoredMessageView;
use crate::sync::fake_server::{Checkout, FakeServer, World, checkout, handle, wire_id};

/// The value of a letter's header, when it has one.
fn header_of(bytes: &[u8], name: &str) -> Option<String> {
    let parsed = mail_parser::MessageParser::default().parse(bytes)?;
    parsed
        .headers()
        .iter()
        .find(|header| header.name().eq_ignore_ascii_case(name))
        .and_then(|header| match header.value() {
            mail_parser::HeaderValue::Text(text) => Some(text.trim().to_string()),
            _ => None,
        })
}

/// The `X-Vivi-Event` value of a letter's bytes.
fn event_of(bytes: &[u8]) -> Option<String> {
    header_of(bytes, "X-Vivi-Event")
}

/// The `X-Vivi-Event-State` value of a letter's bytes.
fn state_of(bytes: &[u8]) -> Option<String> {
    header_of(bytes, "X-Vivi-Event-State")
}

/// The row on `checkout` whose subject is `subject`.
fn held_row(checkout: &Checkout, subject: &str) -> StoredMessageView {
    checkout
        .rows()
        .into_iter()
        .find(|row| row.subject == subject)
        .unwrap_or_else(|| panic!("no row for {subject}"))
}

/// The folder and seal of the row on `checkout` whose subject is `subject`.
fn item_row(checkout: &Checkout, subject: &str) -> (String, bool) {
    let row = held_row(checkout, subject);
    (row.local_role, row.absorbed_at.is_some())
}

/// Give `checkout`'s new `role` kind `mind`, so it may close any item.
fn may_close(checkout: &mut Checkout, role: &str) {
    checkout
        .mailspace
        .add_role(
            role,
            RoleUpdate {
                kind: Some(Some("mind".into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
}

/// Turn the operator ruling's quiet wait off on `mailspace`.
fn quiet_off(mailspace: &mut Mailspace) {
    mailspace.config.absorb = Some(AbsorbConfig { quiet_hours: 0 });
}

/// A need on bob's `lead` (kind `mind`, so it may close for everyone)
/// whose done event letter is stamped `age` old.
fn need_done_age(age: chrono::Duration) -> (World, String) {
    let mut w = World::new();
    may_close(&mut w.bob, "lead");
    w.put_full(
        &wire_id(1, 0),
        "need",
        &w.bob.address("lead"),
        "quiet",
        &need_body(&["x"], ""),
        crate::sync::fake_server::LETTER_DATE,
        "X-Vivi-Format: 1\r\nX-Vivi-Kind: need\r\n",
    );
    w.bob.pull(&w.bob_client);
    let at = (chrono::Utc::now() - age).to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let item = format!("{}@x.test", wire_id(1, 0));
    w.put_full(
        &wire_id(2, 0),
        "mail",
        &w.bob.address("lead"),
        "Re: quiet",
        "done",
        crate::sync::fake_server::LETTER_DATE,
        &format!(
            "In-Reply-To: <{item}>\r\nX-Vivi-Event: done\r\nX-Vivi-Event-Of: <{item}>\r\n\
             X-Vivi-Event-At: {at}\r\n"
        ),
    );
    w.bob.pull(&w.bob_client);
    (w, handle(1))
}

#[test]
fn an_absorb_after_the_quiet_period_succeeds_with_the_default_24_hours() {
    let (w, need) = need_done_age(chrono::Duration::hours(25));

    let absorbed = w.bob.mailspace.absorb("lead", &need, None, "need").unwrap();

    assert_eq!(absorbed, need);
    let sealed = w
        .bob
        .rows()
        .into_iter()
        .map(|row| w.bob.bytes_of(&row.message_id))
        .find(|bytes| event_of(bytes).as_deref() == Some("absorbed"))
        .expect("the absorb wrote a letter");
    assert_eq!(state_of(&sealed).as_deref(), Some("done"));
}

#[test]
fn an_absorb_inside_the_quiet_period_names_the_time_left() {
    let (w, need) = need_done_age(chrono::Duration::hours(20) + chrono::Duration::minutes(48));

    let refused = w
        .bob
        .mailspace
        .absorb("lead", &need, None, "need")
        .unwrap_err()
        .to_string();

    assert!(
        refused.contains("is done but not quiet yet: 3h 12m left"),
        "{refused}"
    );
    assert!(refused.contains("quiet period 24h"), "{refused}");
}

#[test]
fn the_authors_absorb_of_its_sent_row_names_done_and_members_seal_done() {
    let server = FakeServer::new();
    let mut a = checkout("alice", &["mind"]);
    let b = checkout("bob", &["hand"]);
    let c = checkout("carol", &["hand"]);
    quiet_off(&mut a.mailspace);
    // The local cc keeps the author's own copy a `sent` row when the item
    // closes; absorbing it must still name the folded state, done.
    let sent = a
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![b.address("hand"), c.address("hand")],
            cc: vec![a.address("mind")],
            bcc: Vec::new(),
            subject: "folded".into(),
            body: task_body("it works\n"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap()
        .sent;
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    c.pull(&c.client(&server));

    a.mailspace
        .move_item("mind", &sent, "done", None, "task done", None)
        .unwrap();
    let author_row = a.row(&a.id_of(&sent)).unwrap();
    assert_eq!(
        author_row.local_role, "sent",
        "the author's copy stays a sent row"
    );
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    c.pull(&c.client(&server));

    a.mailspace.absorb("mind", &sent, None, "task").unwrap();
    let sealed = a
        .rows()
        .into_iter()
        .map(|row| a.bytes_of(&row.message_id))
        .find(|bytes| event_of(bytes).as_deref() == Some("absorbed"))
        .expect("the absorb wrote a letter");
    assert_eq!(state_of(&sealed).as_deref(), Some("done"), "never open");

    a.push(&a.client(&server));
    // Separate pulls: each member seals its own copy on its own.
    b.pull(&b.client(&server));
    c.pull(&c.client(&server));

    assert_eq!(item_row(&b, "folded"), ("done".into(), true));
    assert_eq!(item_row(&c, "folded"), ("done".into(), true), "never open");
}

/// Two members that may close absorb the same done item, blind to each
/// other, and their letters are pushed in the order `push_first` names.
/// Returns the third member's sealed row: the folder and the seal.
fn converged_row(push_first: &str) -> (String, bool) {
    let server = FakeServer::new();
    let mut a = checkout("alice", &["mind"]);
    let mut b = checkout("bob", &["mind"]);
    let c = checkout("carol", &["hand"]);
    may_close(&mut b, "lead");
    quiet_off(&mut a.mailspace);
    quiet_off(&mut b.mailspace);
    let sent = a
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![b.address("lead")],
            cc: vec![c.address("hand")],
            bcc: Vec::new(),
            subject: "converge".into(),
            body: task_body("it works\n"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap()
        .sent;
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    c.pull(&c.client(&server));

    a.mailspace
        .move_item("mind", &sent, "done", None, "task done", None)
        .unwrap();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    c.pull(&c.client(&server));

    // Both close and both absorb, each blind to the other's letter.
    a.mailspace.absorb("mind", &sent, None, "task").unwrap();
    let b_row = held_row(&b, "converge").handle;
    b.mailspace.absorb("lead", &b_row, None, "task").unwrap();
    for checkout in [&a, &b] {
        let letter = checkout
            .rows()
            .into_iter()
            .map(|row| checkout.bytes_of(&row.message_id))
            .find(|bytes| event_of(bytes).as_deref() == Some("absorbed"))
            .expect("each absorb wrote a letter");
        assert_eq!(state_of(&letter).as_deref(), Some("done"));
    }

    // The third member pulls both letters, in the pushed order.
    let (first, second) = if push_first == "author" {
        (&a, &b)
    } else {
        (&b, &a)
    };
    first.push(&first.client(&server));
    c.pull(&c.client(&server));
    second.push(&second.client(&server));
    c.pull(&c.client(&server));

    item_row(&c, "converge")
}

#[test]
fn two_concurrent_absorbs_seal_the_same_rows_whichever_letter_lands_first() {
    let author_first = converged_row("author");
    let member_first = converged_row("member");

    assert_eq!(author_first, ("done".into(), true));
    assert_eq!(member_first, ("done".into(), true));
    assert_eq!(
        author_first, member_first,
        "identical sealed rows whichever letter the member pulls first"
    );
}
