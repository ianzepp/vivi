//! `keep_local` end to end over the fake server: a stuck letter's push hint,
//! clearing it, undo, and the letters that keep travelling around it.

use crate::storage::MessageIngestRequest;
use crate::sync::fake_server::{Checkout, FakeServer, checkout};
use crate::sync::kept::{KeptLocal, keep_local};
use crate::sync::push;
use crate::sync::sync_dir;

fn alice() -> Checkout {
    checkout("alice", &["mind", "hand"])
}

fn eml(subject: &str, total: Option<usize>) -> Vec<u8> {
    let head = format!(
        "From: mind@x.test\r\nTo: hand@x.test\r\nSubject: {subject}\r\n\
         Message-ID: <{subject}@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\n"
    );
    let pad = total.map_or(String::from("body\r\n"), |n| "x".repeat(n - head.len()));
    format!("{head}{pad}").into_bytes()
}

fn eml_from(subject: &str, from: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: hand@x.test\r\nSubject: {subject}\r\n\
         Message-ID: <{subject}@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\nbody\r\n"
    )
    .into_bytes()
}

/// Store a letter in `role` directly; returns its message id and handle.
fn put(fx: &Checkout, role: &str, bytes: &[u8]) -> (String, String) {
    let stored = fx
        .mailspace
        .storage()
        .unwrap()
        .ingest_message(
            &MessageIngestRequest {
                account: "mind".into(),
                local_role: role.into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("kept\0{role}\0{}", bytes.len()),
                remote: None,
            },
            bytes,
        )
        .unwrap();
    let handle = fx.row(&stored.message_id).unwrap().handle;
    (stored.message_id, handle)
}

fn kept_file(fx: &Checkout) -> KeptLocal {
    KeptLocal::load(&sync_dir(&fx.mailspace.dir)).unwrap()
}

#[test]
fn an_oversize_letter_is_hinted_then_kept_then_failed_again_by_undo() {
    let server = FakeServer::new();
    let a = alice();
    let client = a.client(&server);
    let (id, stuck) = put(&a, "inbox", &eml("too-big", Some(65_537)));

    let report = push::push(&a.mailspace, &a.settings, &client).unwrap();

    assert!(server.items().is_empty(), "the letter never leaves");
    let lines = report.lines().join("\n");
    assert!(lines.contains(&format!("skipped {stuck}")), "{lines}");
    assert!(
        lines.contains(&format!("run: vivi sync keep-local {stuck}")),
        "{lines}"
    );
    assert!(report.failure().unwrap().contains("never be delivered"));

    let out = keep_local(&a.mailspace, &a.settings, &[stuck.clone()], false).unwrap();

    assert_eq!(
        out,
        vec![format!(
            "kept local {stuck}: 65537 bytes, over the 64 KiB cap; not uploaded; it stays in \
             this mailspace and push no longer tries it"
        )]
    );
    assert_eq!(kept_file(&a).len(), 1);
    assert!(kept_file(&a).contains(&a.row(&id).unwrap().content_id));
}

#[test]
fn after_keeping_the_next_push_is_clean_and_a_second_keep_is_idempotent() {
    let server = FakeServer::new();
    let a = alice();
    let client = a.client(&server);
    let (_, stuck) = put(&a, "inbox", &eml("too-big", Some(65_537)));

    keep_local(&a.mailspace, &a.settings, &[stuck.clone()], false).unwrap();

    let report = push::push(&a.mailspace, &a.settings, &client).unwrap();
    assert_eq!(report.sent, 0);
    assert!(report.failure().is_none(), "{:?}", report.lines());
    let summary = report.lines()[0].clone();
    assert!(
        summary.contains("not synced by design: 1 kept local"),
        "{summary}"
    );
    assert_eq!(
        push::pending(&a.mailspace, &a.settings).unwrap().unsent(),
        0
    );

    let again = keep_local(&a.mailspace, &a.settings, &[stuck.clone()], false).unwrap();
    assert_eq!(again.len(), 1);
    assert!(again[0].starts_with(&format!("kept local {stuck}: ")));

    let undone = keep_local(&a.mailspace, &a.settings, &[stuck.clone()], true).unwrap();
    assert_eq!(
        undone,
        vec![format!("no longer kept local {stuck}; push tries it again")]
    );
    let report = push::push(&a.mailspace, &a.settings, &client).unwrap();
    assert!(
        report.failure().unwrap().contains("never be delivered"),
        "{:?}",
        report.lines()
    );
}

#[test]
fn a_foreign_from_letter_is_hinted_then_kept_the_same_way() {
    let server = FakeServer::new();
    let a = alice();
    let client = a.client(&server);
    let (_, stuck) = put(&a, "inbox", &eml_from("forged", "mind@3f9a1c7e.local"));

    let report = push::push(&a.mailspace, &a.settings, &client).unwrap();

    let lines = report.lines().join("\n");
    assert!(lines.contains(&format!("skipped {stuck}")), "{lines}");
    assert!(lines.contains("claims mailspace 3f9a1c7e"), "{lines}");
    assert!(
        lines.contains(&format!("run: vivi sync keep-local {stuck}")),
        "{lines}"
    );
    assert!(report.failure().is_some());

    keep_local(&a.mailspace, &a.settings, &[stuck.clone()], false).unwrap();

    let report = push::push(&a.mailspace, &a.settings, &client).unwrap();
    assert!(report.failure().is_none(), "{:?}", report.lines());
    let summary = report.lines()[0].clone();
    assert!(
        summary.contains("not synced by design: 1 kept local"),
        "{summary}"
    );
    assert_eq!(
        push::pending(&a.mailspace, &a.settings).unwrap().unsent(),
        0
    );
}

#[test]
fn keep_local_refuses_a_letter_push_would_send_and_writes_nothing() {
    let server = FakeServer::new();
    let a = alice();
    let client = a.client(&server);
    let sent = a.send_need("mind", "hand", "plain work");

    let error = keep_local(&a.mailspace, &a.settings, &[sent], false)
        .unwrap_err()
        .to_string();

    assert!(
        error.contains(
            "is not a letter push refuses (too large, or a From that claims another \
                       mailspace); nothing to keep local"
        ),
        "{error}"
    );
    assert!(!sync_dir(&a.mailspace.dir).join("kept-local.json").exists());

    let report = push::push(&a.mailspace, &a.settings, &client).unwrap();
    assert_eq!(report.sent, 1, "the letter still travels");
    assert!(report.failure().is_none());
}

#[test]
fn undoing_a_letter_that_was_never_kept_says_so_without_an_error() {
    let a = alice();
    let (_, stuck) = put(&a, "inbox", &eml("too-big", Some(65_537)));

    let out = keep_local(&a.mailspace, &a.settings, &[stuck.clone()], true).unwrap();

    assert_eq!(
        out,
        vec![format!("not kept local {stuck}; nothing to undo")]
    );
    assert!(!sync_dir(&a.mailspace.dir).join("kept-local.json").exists());
}

#[test]
fn a_normal_letter_still_travels_in_the_same_push_as_a_kept_one() {
    let server = FakeServer::new();
    let a = alice();
    let client = a.client(&server);
    let (_, stuck) = put(&a, "inbox", &eml("too-big", Some(65_537)));
    let work = a.send_need("mind", "hand", "plain work");

    keep_local(&a.mailspace, &a.settings, &[stuck.clone()], false).unwrap();

    let report = push::push(&a.mailspace, &a.settings, &client).unwrap();
    let ids: Vec<String> = server.items().into_iter().map(|i| i.message_id).collect();
    assert_eq!(ids, vec![a.id_of(&work)], "only the sendable letter goes");
    assert_eq!(report.sent, 1);
    assert!(report.failure().is_none(), "{:?}", report.lines());
    let summary = report.lines()[0].clone();
    assert!(
        summary.contains("not synced by design: 1 kept local"),
        "{summary}"
    );
}

#[test]
fn more_stuck_letters_than_are_listed_still_name_the_command() {
    let server = FakeServer::new();
    let a = alice();
    let client = a.client(&server);
    for n in 0..11 {
        put(&a, "inbox", &eml(&format!("too-big-{n}"), Some(65_537 + n)));
    }

    let report = push::push(&a.mailspace, &a.settings, &client).unwrap();

    let lines = report.lines();
    let detailed = lines
        .iter()
        .filter(|line| line.starts_with("skipped "))
        .count();
    assert_eq!(detailed, 10, "only ten handles are listed: {lines:?}");
    assert!(
        lines.iter().any(|line| line.contains(
            "... and 1 more skipped; run vivi sync keep-local <handle> \
                                       for each"
        )),
        "{lines:?}"
    );
}
