use super::*;
use crate::storage::MessageIngestRequest;

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    (tmp, mailspace)
}

/// File a letter under a raw account as a pull does, minting no node.
fn hold(mailspace: &Mailspace, account: &str, role: &str, kind: &str, subject: &str) -> String {
    let bytes = format!(
        "From: mind@{domain}\r\nTo: {account}@{domain}\r\nSubject: {subject}\r\n\
         Message-ID: <{subject}@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\
         X-Vivi-Kind: {kind}\r\n\r\ndone_when: {subject}\r\n",
        domain = mailspace.address_domain()
    );
    let storage = mailspace.storage().unwrap();
    let stored = storage
        .ingest_message(
            &MessageIngestRequest {
                account: account.into(),
                local_role: role.into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("{account}\0{subject}"),
                remote: None,
            },
            bytes.as_bytes(),
        )
        .unwrap();
    storage.display_handle(&stored.message_id).unwrap()
}

#[test]
fn letters_under_a_name_no_role_carries_are_listed_with_counts() {
    let (_tmp, mailspace) = space();
    hold(&mailspace, "ghost", "tasks", "task", "one");
    hold(&mailspace, "ghost", "inbox", "mail", "two");
    hold(&mailspace, "phantom", "needs", "need", "three");
    hold(&mailspace, "mind", "inbox", "mail", "ours");
    hold(&mailspace, "hand@3f9a1c7e.local", "needs", "need", "theirs");

    let held = mailspace.held_for_unknown_roles().unwrap();

    assert_eq!(
        held,
        [("ghost".to_string(), 2), ("phantom".to_string(), 1)],
        "own roles and other mailspaces' addresses are not unknown roles"
    );
}

#[test]
fn a_legacy_node_for_an_unknown_role_is_not_dispatched() {
    let (_tmp, mailspace) = space();
    let handle = hold(&mailspace, "ghost", "tasks", "task", "legacy");
    mailspace
        .backlog_attach("task", &handle, "legacy", &[])
        .unwrap();

    let manifest = mailspace.step_shadow().unwrap();
    assert!(manifest.dispatches.is_empty(), "{manifest:?}");
    let node = mailspace
        .graph_show("backlog")
        .unwrap()
        .nodes
        .into_iter()
        .find(|node| node.source_id == handle)
        .unwrap();
    assert_eq!(node.readiness, "unowned");
    let err = mailspace
        .graph_activate("backlog", &handle, &handle, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown role 'ghost'"), "{err}");
}

#[test]
fn adding_the_role_adopts_held_letters_and_step_then_dispatches_them() {
    let (_tmp, mut mailspace) = space();
    hold(&mailspace, "ghost", "tasks", "task", "work");
    hold(&mailspace, "ghost", "inbox", "mail", "note");
    assert!(mailspace.step_shadow().unwrap().dispatches.is_empty());

    mailspace.add_identity("ghost").unwrap();
    let adopted = mailspace.adopt_held_letters("ghost").unwrap();

    assert_eq!(adopted, 2, "the work letter and the note");
    let manifest = mailspace.step_shadow().unwrap();
    assert_eq!(manifest.dispatches.len(), 1, "{manifest:?}");
    assert!(mailspace.held_for_unknown_roles().unwrap().is_empty());
    assert_eq!(mailspace.adopt_held_letters("ghost").unwrap(), 2);
    assert_eq!(
        mailspace.step_shadow().unwrap().dispatches.len(),
        1,
        "adopting twice mints nothing twice"
    );
}

#[test]
fn renaming_a_role_to_a_held_name_adopts_the_same_way() {
    let (_tmp, mut mailspace) = space();
    mailspace.add_identity("hand").unwrap();
    hold(&mailspace, "scribe", "needs", "need", "typo'd");

    mailspace.rename_identity("hand", "scribe").unwrap();
    let adopted = mailspace.adopt_held_letters("scribe").unwrap();

    assert_eq!(adopted, 1);
    assert_eq!(mailspace.step_shadow().unwrap().dispatches.len(), 1);
}
