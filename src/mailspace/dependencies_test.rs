use super::*;
use crate::letter::fixture::{need_body_with, task_body_with};
use crate::letter::work::{NeedFront, TaskFront};
use crate::storage::MessageIngestRequest;

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("hand").unwrap();
    (tmp, mailspace)
}

/// A work letter as another mailspace's sender composed it: the headers name
/// the prerequisites, whatever this mailspace holds.
fn letter(mailspace: &Mailspace, subject: &str, kind: &str, deps: &[&str]) -> Vec<u8> {
    let depends_on: Vec<String> = deps.iter().map(ToString::to_string).collect();
    let done_when = vec![subject.to_string()];
    let body = if kind == "need" {
        let mut front = NeedFront::new(done_when);
        front.depends_on = depends_on;
        need_body_with(&front, "")
    } else {
        task_body_with(
            &TaskFront {
                done_when,
                depends_on,
                ..TaskFront::default()
            },
            "",
        )
    };
    format!(
        "From: mind@{domain}\r\nTo: hand@{domain}\r\nSubject: {subject}\r\n\
         Message-ID: <{subject}@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\
         X-Vivi-Format: 1\r\nX-Vivi-Kind: {kind}\r\n\r\n{body}",
        domain = mailspace.address_domain()
    )
    .into_bytes()
}

/// File a letter under `account` the way a pull does, then attach it. Returns
/// its message id and display handle.
fn file(
    mailspace: &Mailspace,
    account: &str,
    kind: &str,
    subject: &str,
    deps: &[&str],
) -> (String, String) {
    let bytes = letter(mailspace, subject, kind, deps);
    let role = format!("{kind}s");
    let storage = mailspace.storage().unwrap();
    let stored = storage
        .ingest_message(
            &MessageIngestRequest {
                account: account.into(),
                local_role: role,
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("{account}\0{subject}"),
                remote: None,
            },
            &bytes,
        )
        .unwrap();
    let handle = storage.display_handle(&stored.message_id).unwrap();
    drop(storage);
    if !mailspace.is_unowned_account(account) && !is_foreign_account(account) {
        mailspace
            .backlog_attach_letter(kind, &stored.message_id, subject, &bytes)
            .unwrap();
        let arrived = (stored.message_id.clone(), stored.content_id.clone());
        mailspace
            .backlog_settle_pending_dependencies(&[arrived])
            .unwrap();
    }
    (stored.message_id, handle)
}

fn node_of(mailspace: &Mailspace, item: &str) -> Option<crate::mailspace::GraphNodeView> {
    mailspace
        .graph_show("backlog")
        .ok()?
        .nodes
        .into_iter()
        .find(|node| node.source_id == item)
}

fn readiness(mailspace: &Mailspace, item: &str) -> String {
    node_of(mailspace, item)
        .unwrap_or_else(|| panic!("no node for {item}"))
        .readiness
}

fn edge_count(mailspace: &Mailspace) -> usize {
    mailspace.graph_show("backlog").unwrap().edges.len()
}

fn blocked_listing(mailspace: &Mailspace) -> Vec<String> {
    mailspace
        .list_tasks_with_deps("hand", true, None)
        .unwrap()
        .into_iter()
        .map(|(view, _)| view.subject)
        .collect()
}

#[test]
fn blocked_lists_the_unfinished_prerequisite_and_filters_by_kind() {
    let (_tmp, mailspace) = space();
    let (_, need) = file(&mailspace, "hand", "need", "first", &[]);
    let (_, task) = file(&mailspace, "hand", "task", "second", &[&need]);

    let blocked = mailspace.graph_blocked("backlog", Some("task")).unwrap();
    let row = blocked
        .iter()
        .find(|node| node.source_id == task)
        .expect("the dependent task is blocked");
    assert_eq!(row.blocked_by, vec![need.clone()]);

    let needs = mailspace.graph_blocked("backlog", Some("need")).unwrap();
    assert!(needs.iter().all(|node| node.kind == "need"));
    assert!(needs.iter().all(|node| node.source_id != task));
}

#[test]
fn a_held_dependency_becomes_an_edge_and_the_dependent_waits() {
    let (_tmp, mailspace) = space();
    let (_, need) = file(&mailspace, "hand", "need", "first", &[]);
    let (_, task) = file(&mailspace, "hand", "task", "second", &[&need]);

    assert_eq!(readiness(&mailspace, &need), "ready");
    assert_eq!(readiness(&mailspace, &task), "blocked");
    assert_eq!(edge_count(&mailspace), 1);
    assert_eq!(
        blocked_listing(&mailspace),
        ["second"],
        "list agrees with graph"
    );
    assert!(
        mailspace
            .storage()
            .unwrap()
            .pending_dependencies()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_dependency_not_held_yet_waits_and_the_edge_is_made_on_arrival() {
    let (_tmp, mailspace) = space();
    let (_, task) = file(&mailspace, "hand", "task", "second", &["deadbeef"]);

    assert_eq!(
        readiness(&mailspace, &task),
        "blocked",
        "an unresolved reference blocks the dependent"
    );
    let blocked = mailspace.graph_blocked("backlog", Some("task")).unwrap();
    let row = blocked
        .iter()
        .find(|node| node.source_id == task)
        .expect("the dependent task is blocked");
    assert_eq!(row.blocked_by, vec!["deadbeef".to_string()]);
    assert_eq!(
        node_of(&mailspace, &task).unwrap().awaiting_dependencies,
        ["deadbeef".to_string()]
    );
    assert_eq!(edge_count(&mailspace), 0);
    assert_eq!(
        blocked_listing(&mailspace),
        ["second"],
        "an unheld dependency blocks the dependent until it arrives"
    );
    let pending = mailspace.storage().unwrap().pending_dependencies().unwrap();
    assert_eq!(pending.len(), 1, "{pending:?}");

    // The dependency arrives under the id the header names.
    let bytes = letter(&mailspace, "first", "need", &[]);
    let storage = mailspace.storage().unwrap();
    let stored = storage
        .ingest_message(
            &MessageIngestRequest {
                account: "hand".into(),
                local_role: "needs".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: Some("msg_deadbeef00000000aaaaaaaa".into()),
                seed_hint: "arrival".into(),
                remote: None,
            },
            &bytes,
        )
        .unwrap();
    drop(storage);
    mailspace
        .backlog_attach_letter("need", &stored.message_id, "first", &bytes)
        .unwrap();
    let made = mailspace
        .backlog_settle_pending_dependencies(&[(stored.message_id.clone(), stored.content_id)])
        .unwrap();

    assert_eq!(made, 1);
    assert_eq!(edge_count(&mailspace), 1);
    let dep = mailspace
        .storage()
        .unwrap()
        .display_handle(&stored.message_id)
        .unwrap();
    let dep = node_of(&mailspace, &dep).map_or(dep, |node| node.source_id);
    assert_eq!(readiness(&mailspace, &task), "blocked");
    let row = node_of(&mailspace, &task).unwrap();
    assert_eq!(row.blocked_by, vec![dep.clone()], "the arrived open item");
    assert!(
        row.awaiting_dependencies.is_empty(),
        "a held, open dependency is an edge, not an unresolved reference: {:?}",
        row.awaiting_dependencies
    );
    assert_eq!(blocked_listing(&mailspace), ["second"]);
    let pending = mailspace.storage().unwrap().pending_dependencies().unwrap();
    assert!(pending.is_empty(), "{pending:?}");

    // Once the dependency arrives and is done, the dependent becomes ready.
    mailspace.backlog_complete_item(&dep).unwrap();
    assert_eq!(node_of(&mailspace, &dep).unwrap().state, "done");
    assert_eq!(readiness(&mailspace, &task), "ready");
}

#[test]
fn a_reference_that_can_never_resolve_is_remembered_and_unmet() {
    let (_tmp, mailspace) = space();
    let (_, task) = file(&mailspace, "hand", "task", "second", &["T1"]);

    assert_eq!(
        readiness(&mailspace, &task),
        "blocked",
        "a reference that can never resolve still blocks"
    );
    let row = node_of(&mailspace, &task).unwrap();
    assert!(
        row.blocked_by.iter().any(|token| token == "T1"),
        "blocked_by names the raw reference: {:?}",
        row.blocked_by
    );
    assert_eq!(row.awaiting_dependencies, ["T1".to_string()]);
    let pending = mailspace.storage().unwrap().pending_dependencies().unwrap();
    assert!(
        pending.iter().flat_map(|(_, refs)| refs).any(|r| r == "T1"),
        "{pending:?}"
    );
    assert_eq!(
        blocked_listing(&mailspace),
        ["second"],
        "an unresolved reference blocks the dependent"
    );
}

#[test]
fn dependency_satisfied_wants_a_held_done_row() {
    let (_tmp, mailspace) = space();
    assert!(
        !dependency_satisfied(&mailspace.storage().unwrap(), "deadbeef"),
        "an unknown token is not satisfied"
    );
    let (_, first) = file(&mailspace, "hand", "need", "first", &[]);
    assert!(
        !dependency_satisfied(&mailspace.storage().unwrap(), &first),
        "a held open row is not satisfied"
    );
    mailspace
        .storage()
        .unwrap()
        .move_message_to_role("hand", &first, "done")
        .unwrap();
    assert!(
        dependency_satisfied(&mailspace.storage().unwrap(), &first),
        "a held row moved to done is satisfied"
    );
}

#[test]
fn a_dependency_held_for_another_mailspace_makes_no_edge() {
    let (_tmp, mailspace) = space();
    let (_, theirs) = file(&mailspace, "hand@3f9a1c7e.local", "need", "theirs", &[]);
    let (_, task) = file(&mailspace, "hand", "task", "ours", &[&theirs]);

    assert!(
        node_of(&mailspace, &theirs).is_none(),
        "foreign work mints no node"
    );
    assert_eq!(
        readiness(&mailspace, &task),
        "blocked",
        "a foreign-held reference blocks until it is present and done"
    );
    assert!(
        node_of(&mailspace, &task)
            .unwrap()
            .blocked_by
            .contains(&theirs),
        "blocked_by names the foreign-held reference"
    );
    assert_eq!(edge_count(&mailspace), 0);
    let pending = mailspace.storage().unwrap().pending_dependencies().unwrap();
    assert_eq!(
        pending.iter().flat_map(|(_, refs)| refs).count(),
        1,
        "a foreign-held reference is remembered as unresolved: {pending:?}"
    );
    assert_eq!(
        blocked_listing(&mailspace),
        ["ours"],
        "it blocks until the named work is present and done"
    );
}

#[test]
fn a_letter_held_for_another_mailspace_gets_no_edge() {
    let (_tmp, mailspace) = space();
    let (_, need) = file(&mailspace, "hand", "need", "ours", &[]);
    let (_, theirs) = file(
        &mailspace,
        "hand@3f9a1c7e.local",
        "task",
        "theirs",
        &[&need],
    );

    assert!(node_of(&mailspace, &theirs).is_none());
    assert_eq!(edge_count(&mailspace), 0);
}

#[test]
fn headers_that_would_close_a_cycle_or_order_a_node_after_itself_are_ignored() {
    let (_tmp, mailspace) = space();
    let (first_id, first) = file(&mailspace, "hand", "need", "first", &[]);
    let (_, second) = file(&mailspace, "hand", "task", "second", &[&first]);
    let (third_id, third) = file(&mailspace, "hand", "task", "third", &[&second]);
    assert_eq!(edge_count(&mailspace), 2);

    mailspace.link_dependencies(&third_id, &[third]).unwrap();
    assert_eq!(edge_count(&mailspace), 2, "no self edge");
    // first -> second -> third: `first` after `third` would close the loop.
    mailspace
        .link_dependencies(&first_id, &[second.clone()])
        .unwrap();
    assert_eq!(edge_count(&mailspace), 2, "the cycle edge was refused");
    assert_eq!(readiness(&mailspace, &first), "ready");
}

#[test]
fn audit_reports_header_drift_and_repair_makes_the_edge() {
    let (_tmp, mailspace) = space();
    let (_, need) = file(&mailspace, "hand", "need", "first", &[]);
    // A node minted without its header edges: the state a pull left before
    // edges were rebuilt.
    let bytes = letter(&mailspace, "second", "task", &[&need]);
    let storage = mailspace.storage().unwrap();
    let stored = storage
        .ingest_message(
            &MessageIngestRequest {
                account: "hand".into(),
                local_role: "tasks".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: "drift".into(),
                remote: None,
            },
            &bytes,
        )
        .unwrap();
    let task = storage.display_handle(&stored.message_id).unwrap();
    drop(storage);
    mailspace
        .backlog_attach("task", &task, "second", &[])
        .unwrap();
    assert_eq!(readiness(&mailspace, &task), "ready", "the drift");
    assert_eq!(
        blocked_listing(&mailspace),
        ["second"],
        "header says blocked"
    );

    let report = mailspace.backlog_audit(false).unwrap();
    let edges: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.class == "missing_edge")
        .collect();
    assert_eq!(edges.len(), 1, "{:?}", report.findings);
    assert_eq!(edges[0].handle, task);
    assert!(!edges[0].repaired);
    assert_eq!(edge_count(&mailspace), 0, "a plain audit changes nothing");

    let repaired = mailspace.backlog_audit(true).unwrap();
    assert!(repaired.findings.iter().all(|f| f.repaired));
    assert_eq!(readiness(&mailspace, &task), "blocked");
    let after = mailspace.backlog_audit(false).unwrap();
    assert!(after.findings.is_empty(), "{:?}", after.findings);
}

#[test]
fn deliver_raw_orders_the_work_without_repair() {
    let (_tmp, mailspace) = space();
    // A graph exists, so the audit has candidates.
    let (_, seed) = file(&mailspace, "hand", "task", "seed", &[]);
    assert!(node_of(&mailspace, &seed).is_some());
    let bytes = letter(&mailspace, "first", "need", &[]);
    mailspace.deliver_raw(&bytes, "needs").unwrap();
    let first = mailspace
        .list_kind(Some("hand"), "needs", "need")
        .unwrap()
        .remove(0)
        .handle;
    mailspace
        .deliver_raw(&letter(&mailspace, "second", "task", &[&first]), "tasks")
        .unwrap();

    let task = mailspace
        .list_kind(Some("hand"), "tasks", "task")
        .unwrap()
        .into_iter()
        .find(|view| view.subject == "second")
        .unwrap()
        .handle;
    assert_eq!(readiness(&mailspace, &first), "ready");
    assert_eq!(readiness(&mailspace, &task), "blocked");
    assert!(mailspace.backlog_audit(false).unwrap().findings.is_empty());
}

#[test]
fn adoption_attaches_held_work_with_its_edges() {
    let (_tmp, mut mailspace) = space();
    let (_, first) = file(&mailspace, "ghost", "need", "first", &[]);
    let (_, second) = file(&mailspace, "ghost", "task", "second", &[&first]);
    assert!(node_of(&mailspace, &first).is_none(), "held, not backlog");
    assert!(node_of(&mailspace, &second).is_none());

    mailspace.add_identity("ghost").unwrap();
    let adopted = mailspace.adopt_held_letters("ghost").unwrap();

    assert_eq!(adopted, 2);
    assert_eq!(readiness(&mailspace, &first), "ready");
    assert_eq!(readiness(&mailspace, &second), "blocked");
}

/// A `dependency-added` / `dependency-removed` letter on `item_rfc` naming
/// `dep_rfc`, written by `mind` at `at` (RFC 3339), as another member's
/// letter arrives.
fn dependency_letter(
    mailspace: &Mailspace,
    kind: &str,
    item_rfc: &str,
    dep_rfc: &str,
    at: &str,
) -> Vec<u8> {
    format!(
        "From: mind@{domain}\r\nTo: hand@{domain}\r\nSubject: Re: item\r\n\
         Message-ID: <{kind}-{at}@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\
         In-Reply-To: <{item_rfc}>\r\nReferences: <{item_rfc}>\r\n\
         X-Vivi-Kind: mail\r\nX-Vivi-Event: {kind}\r\nX-Vivi-Event-Of: <{item_rfc}>\r\n\
         X-Vivi-Event-At: {at}\r\nX-Vivi-Event-Dep: <{dep_rfc}>\r\n\r\n{kind}\r\n",
        domain = mailspace.address_domain()
    )
    .into_bytes()
}

fn arrive_event(mailspace: &Mailspace, bytes: &[u8], item_rfc: &str, seed: &str) {
    let storage = mailspace.storage().unwrap();
    let parent = storage.live_message_by_rfc_id(item_rfc).unwrap().unwrap();
    let written = storage
        .apply_item_event(&crate::storage::ItemEventWrite {
            reply_requests: &[MessageIngestRequest {
                account: "hand".into(),
                local_role: "inbox".into(),
                read_state: true,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: seed.into(),
                remote: None,
            }],
            reply_data: Some(bytes),
            parent_content_id: &parent.content_id,
            moves: &[],
        })
        .unwrap();
    drop(storage);
    mailspace
        .apply_arrived_events(&[written[0].content_id.clone()])
        .unwrap();
}

#[test]
fn an_added_dependency_not_held_yet_waits_and_the_edge_is_made_on_arrival() {
    let (_tmp, mailspace) = space();
    let (_, release) = file(&mailspace, "hand", "need", "release", &[]);
    assert_eq!(readiness(&mailspace, &release), "ready");

    // The letter names a dependency this mailspace does not hold yet.
    let added = dependency_letter(
        &mailspace,
        "dependency-added",
        "release@x.test",
        "work@x.test",
        "2026-10-11T12:00:01.000000Z",
    );
    arrive_event(&mailspace, &added, "release@x.test", "added");
    assert_eq!(readiness(&mailspace, &release), "blocked");
    assert_eq!(edge_count(&mailspace), 0);
    let waiting = node_of(&mailspace, &release).unwrap().awaiting_dependencies;
    assert_eq!(waiting, ["<work@x.test>".to_string()]);

    // The dependency arrives: the edge is made.
    let (_, work) = file(&mailspace, "hand", "need", "work", &[]);
    assert_eq!(edge_count(&mailspace), 1);
    assert_eq!(readiness(&mailspace, &release), "blocked");
    assert!(
        node_of(&mailspace, &release)
            .unwrap()
            .awaiting_dependencies
            .is_empty()
    );

    // A newer removal drops the edge.
    let removed = dependency_letter(
        &mailspace,
        "dependency-removed",
        "release@x.test",
        "work@x.test",
        "2026-10-11T12:00:02.000000Z",
    );
    arrive_event(&mailspace, &removed, "release@x.test", "removed");
    assert_eq!(edge_count(&mailspace), 0);
    assert_eq!(readiness(&mailspace, &release), "ready");
    assert_eq!(readiness(&mailspace, &work), "ready");
}

#[test]
fn a_format_one_task_letter_names_its_refs_and_parent_in_front_matter() {
    let body = "+++\ndepends_on = [\"000000a1\", \" 000000b2 \", \"000000a1\"]\n\
                parent = \"<source@x.test>\"\n+++\nprose\n";
    let bytes = format!(
        "From: mind@a.local\r\nTo: hand@a.local\r\nSubject: t\r\n\
         Message-ID: <t@x.test>\r\nX-Vivi-Format: 1\r\nX-Vivi-Kind: task\r\n\
         X-Vivi-Depends-On: 000000ff\r\n\r\n{body}"
    )
    .into_bytes();

    assert_eq!(depends_on_refs(&bytes), ["000000a1", "000000b2"]);
    assert_eq!(parent_ref(&bytes).as_deref(), Some("source@x.test"));
}

// legacy: flips to letter_invalid in W3B2-39
#[test]
fn an_old_letter_still_names_its_refs_in_headers() {
    let bytes = b"From: mind@a.local\r\nTo: hand@a.local\r\nSubject: t\r\n\
                  Message-ID: <t@x.test>\r\nX-Vivi-Kind: task\r\n\
                  X-Vivi-Depends-On: 000000a1\r\nX-Vivi-Parent: <p@x.test>\r\n\r\nbody\r\n";

    assert_eq!(depends_on_refs(bytes), ["000000a1"]);
    assert_eq!(parent_ref(bytes).as_deref(), Some("p@x.test"));
}
