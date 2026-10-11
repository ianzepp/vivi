use super::*;
use crate::mailspace::{
    DumpFilters, SendRequest, TaskDumpRequest, TaskDumpStatus, WantListOptions, frontier_from_show,
};

const REMOTE: &str = "hand@3f9a1c7e.local";
const BODY: &str = "done_when: it works\n";

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("hand").unwrap();
    (tmp, mailspace)
}

fn send_work(
    mailspace: &Mailspace,
    kind: &str,
    to: &str,
    subject: &str,
    depends_on: &[&str],
) -> Result<String, VivariumError> {
    let role = format!("{kind}s");
    let result = mailspace.send(SendRequest {
        from: "mind".into(),
        to: vec![to.into()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: subject.into(),
        body: BODY.into(),
        role,
        kind: Some(kind.into()),
        reply_to: None,
        depends_on: depends_on.iter().map(ToString::to_string).collect(),
    })?;
    // The item is the delivered copy when there is one, else the sent copy.
    Ok(result
        .delivered
        .first()
        .map_or(result.sent, |delivered| delivered.handle.clone()))
}

fn readiness(mailspace: &Mailspace, item: &str) -> String {
    mailspace
        .graph_show("backlog")
        .unwrap()
        .nodes
        .into_iter()
        .find(|node| node.source_id == item)
        .unwrap_or_else(|| panic!("no node for {item}"))
        .readiness
}

#[test]
fn a_sent_copy_of_remote_work_is_tracked_and_never_dispatched() {
    let (_tmp, mailspace) = space();
    let mut sent = Vec::new();
    for kind in ["need", "task", "want"] {
        sent.push((
            kind,
            send_work(&mailspace, kind, REMOTE, kind, &[]).unwrap(),
        ));
    }

    let manifest = mailspace.step_shadow().unwrap();
    assert!(manifest.dispatches.is_empty(), "{manifest:?}");
    for (kind, handle) in &sent {
        let exception = manifest
            .exceptions
            .iter()
            .find(|exception| exception.item == *handle)
            .unwrap_or_else(|| panic!("no exception for the sent {kind}"));
        assert_eq!(exception.reason, "awaiting_remote");
        assert_eq!(exception.kind, *kind, "the node kind, not the folder");
        assert!(exception.detail.contains(REMOTE), "{}", exception.detail);
        assert_eq!(readiness(&mailspace, handle), "awaiting");
    }
    let show = mailspace.graph_show("backlog").unwrap();
    assert!(show.ready.is_empty(), "{:?}", show.ready);
    let frontier = frontier_from_show(&show);
    assert!(frontier.ready.is_empty());
    assert_eq!(frontier.awaiting.len(), 3, "{frontier:?}");
}

#[test]
fn a_local_item_beside_a_sent_copy_still_dispatches() {
    let (_tmp, mailspace) = space();
    let remote = send_work(&mailspace, "task", REMOTE, "away", &[]).unwrap();
    let local = send_work(&mailspace, "task", "hand", "here", &[]).unwrap();

    let manifest = mailspace.step_shadow().unwrap();

    assert_eq!(manifest.dispatches.len(), 1, "{manifest:?}");
    assert_eq!(manifest.dispatches[0].subject, "here");
    assert!(manifest.exceptions.iter().any(|e| e.item == remote));
    assert!(!manifest.exceptions.iter().any(|e| e.item == local));
}

#[test]
fn a_sent_copy_is_a_dependency_and_its_dependents_wait_for_it() {
    let (_tmp, mailspace) = space();
    let first = send_work(&mailspace, "need", REMOTE, "first", &[]).unwrap();

    let second = send_work(&mailspace, "task", REMOTE, "second", &[&first]).unwrap();
    let local = send_work(&mailspace, "task", "hand", "third", &[&first]).unwrap();

    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(&second).unwrap();
    let deps = mailspace.task_depends_on(&id).unwrap();
    assert_eq!(deps, [first.clone()], "the header carries the dependency");
    drop(storage);
    assert_eq!(readiness(&mailspace, &second), "blocked");
    let blocked = mailspace.graph_show("backlog").unwrap().blocked;
    assert!(blocked.iter().any(|node| node.source_id == local));

    mailspace
        .move_item("mind", &first, "done", None, "need done", None)
        .unwrap();

    assert_eq!(readiness(&mailspace, &local), "ready");
    assert_eq!(readiness(&mailspace, &second), "awaiting");
}

#[test]
fn only_work_sent_copies_are_dependencies() {
    let (_tmp, mailspace) = space();
    let sent = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![REMOTE.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "just mail".into(),
            body: "hi".into(),
            role: "inbox".into(),
            kind: Some("mail".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .sent;

    let error = send_work(&mailspace, "task", REMOTE, "after", &[&sent]).unwrap_err();

    assert!(error.to_string().contains("has role 'sent'"), "{error}");
}

#[test]
fn activating_a_sent_copy_names_the_address_it_was_handed_to() {
    let (_tmp, mailspace) = space();
    let task = send_work(&mailspace, "task", REMOTE, "away", &[]).unwrap();

    let error = mailspace
        .graph_activate("backlog", &task, &task, None)
        .unwrap_err();

    let text = error.to_string();
    assert!(
        text.contains("handed to") && text.contains(REMOTE),
        "{text}"
    );
}

#[test]
fn closing_a_sent_copy_completes_its_node_and_the_owner_can_reopen_it() {
    let (_tmp, mailspace) = space();
    let need = send_work(&mailspace, "need", REMOTE, "away", &[]).unwrap();
    let want = send_work(&mailspace, "want", REMOTE, "maybe", &[]).unwrap();

    // Promoting or reopening an open sent copy would land a second owner
    // here; it stays refused.
    for (handle, role) in [(&need, "needs"), (&want, "needs")] {
        let error = mailspace
            .move_item("mind", handle, role, None, "need reopen", None)
            .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("only the receiving mailspace"), "{text}");
        assert!(text.contains(REMOTE), "{text}");
    }
    assert_eq!(mailspace.list("mind", "sent").unwrap().len(), 2);
    assert!(mailspace.list("mind", "needs").unwrap().is_empty());

    mailspace
        .move_item("mind", &need, "done", None, "need done", None)
        .unwrap();

    assert_eq!(readiness(&mailspace, &need), "n/a");
    let node_state = |mailspace: &Mailspace| {
        mailspace
            .graph_show("backlog")
            .unwrap()
            .nodes
            .into_iter()
            .find(|node| node.source_id == need)
            .unwrap()
            .state
    };
    assert_eq!(node_state(&mailspace), "done");
    assert_eq!(mailspace.list("mind", "done").unwrap().len(), 1);

    // The closed copy is the owner's: it reopens it with an event letter,
    // and the copy goes back to `sent`, awaiting the receiving mailspace.
    mailspace
        .move_item("mind", &need, "needs", None, "need reopen", None)
        .unwrap();

    assert_eq!(node_state(&mailspace), "open");
    assert_eq!(readiness(&mailspace, &need), "awaiting");
    assert!(mailspace.list("mind", "done").unwrap().is_empty());
    assert!(mailspace.list("mind", "needs").unwrap().is_empty());
    assert_eq!(mailspace.list("mind", "sent").unwrap().len(), 4);
}

#[test]
fn a_local_sent_copy_of_local_work_still_reopens_its_delivered_copy() {
    let (_tmp, mailspace) = space();
    let result = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "local".into(),
            body: BODY.into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let handle = result.delivered[0].handle.clone();
    mailspace
        .move_item("hand", &handle, "done", None, "task done", None)
        .unwrap();

    mailspace
        .move_item("hand", &handle, "tasks", None, "task reopen", None)
        .unwrap();

    assert_eq!(readiness(&mailspace, &handle), "ready");
}

#[test]
fn the_author_lists_and_dumps_open_outbound_work_only() {
    let (_tmp, mailspace) = space();
    let need = send_work(&mailspace, "need", REMOTE, "away", &[]).unwrap();
    send_work(&mailspace, "need", "hand", "local", &[]).unwrap();
    send_work(&mailspace, "task", REMOTE, "a task", &[]).unwrap();
    let want = send_work(&mailspace, "want", REMOTE, "a want", &[]).unwrap();

    let outbound = mailspace.list_outbound_work(Some("mind"), "need").unwrap();
    assert_eq!(outbound.len(), 1, "{outbound:?}");
    assert_eq!(outbound[0].handle, need);
    assert_eq!(outbound[0].local_role, "sent");
    assert!(
        mailspace
            .list_outbound_work(Some("hand"), "need")
            .unwrap()
            .is_empty()
    );

    let dump = |include_outbound, status| {
        mailspace
            .dump_tasks(TaskDumpRequest {
                status,
                open_role: "needs".into(),
                kind: "need".into(),
                include_outbound,
                filters: DumpFilters {
                    for_identity: Some("mind".into()),
                    ..Default::default()
                },
            })
            .unwrap()
    };
    let records = dump(true, TaskDumpStatus::Open);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].handle, need);
    assert_eq!(records[0].role, "sent");
    assert_eq!(records[0].status.as_deref(), Some("open"));
    assert!(dump(false, TaskDumpStatus::Open).is_empty());
    assert!(dump(true, TaskDumpStatus::Done).is_empty());
    assert_eq!(dump(true, TaskDumpStatus::All).len(), 1);

    let wants = mailspace
        .list_wants_with_metadata(Some("mind"), &["wants", "sent"], WantListOptions::default())
        .unwrap();
    assert_eq!(wants.len(), 1);
    assert_eq!(wants[0].handle, want);
    assert_eq!(wants[0].status, "open");
    assert_eq!(wants[0].role, "sent");

    mailspace
        .move_item("mind", &need, "done", None, "need done", None)
        .unwrap();
    assert!(
        mailspace
            .list_outbound_work(Some("mind"), "need")
            .unwrap()
            .is_empty(),
        "a closed sent copy is no longer outbound"
    );
}

#[test]
fn open_work_content_ids_follow_open_nodes_only() {
    let (_tmp, mailspace) = space();
    let need = send_work(&mailspace, "need", REMOTE, "away", &[]).unwrap();
    let storage = mailspace.storage().unwrap();
    let content = storage
        .message_by_id(&storage.resolve_message_token(&need).unwrap())
        .unwrap()
        .unwrap()
        .content_id;
    drop(storage);
    assert!(
        mailspace
            .open_work_content_ids()
            .unwrap()
            .contains(&content)
    );

    mailspace
        .move_item("mind", &need, "done", None, "need done", None)
        .unwrap();

    assert!(mailspace.open_work_content_ids().unwrap().is_empty());
}

#[test]
fn only_addresses_carry_a_foreign_account() {
    assert!(is_foreign_account("hand@3f9a1c7e.local"));
    assert!(is_foreign_account("postmaster@audit.local"));
    assert!(!is_foreign_account("hand"));
    assert!(!is_foreign_account("ghost"));
}

#[test]
fn stored_address_lists_parse_to_bare_addresses() {
    assert_eq!(
        bare_addresses("Alice <Alice@A.local>, hand@b.local ,"),
        ["alice@a.local", "hand@b.local"]
    );
    assert!(bare_addresses("").is_empty());
}
