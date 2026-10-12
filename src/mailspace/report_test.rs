use super::*;
use crate::mailspace::{RoleUpdate, SendRequest, frontier_from_show};

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    for (name, kind) in [("mind", "mind"), ("hand", "hand"), ("auditor", "auditor")] {
        mailspace.add_identity(name).unwrap();
        mailspace
            .set_role(
                name,
                RoleUpdate {
                    kind: Some(Some(kind.into())),
                    ..RoleUpdate::default()
                },
            )
            .unwrap();
    }
    (tmp, mailspace)
}

/// Send `kind` work from the mind to the hand; the hand's handle.
fn send(mailspace: &Mailspace, kind: &str, subject: &str, depends_on: &[&str]) -> String {
    mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: "done_when: lands\n".into(),
            role: format!("{kind}s"),
            kind: Some(kind.into()),
            reply_to: None,
            depends_on: depends_on.iter().map(ToString::to_string).collect(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

fn done(mailspace: &Mailspace, identity: &str, handle: &str) -> crate::mailspace::MoveOutcome {
    mailspace
        .move_item_outcome(identity, handle, "done", None, "task done", None)
        .unwrap()
}

fn node(mailspace: &Mailspace, source_id: &str) -> GraphNodeView {
    mailspace
        .graph_show("backlog")
        .unwrap()
        .nodes
        .into_iter()
        .find(|n| n.source_id == source_id)
        .unwrap()
}

fn role_of(mailspace: &Mailspace, handle: &str) -> String {
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(handle).unwrap();
    storage.message_by_id(&id).unwrap().unwrap().local_role
}

#[test]
fn a_hands_done_is_a_report_that_leaves_the_node_open_and_dependents_blocked() {
    let (_tmp, mailspace) = space();
    let first = send(&mailspace, "task", "unit A", &[]);
    let second = send(&mailspace, "task", "unit B", &[&first]);

    let outcome = done(&mailspace, "hand", &first);

    assert!(outcome.reported, "{outcome:?}");
    assert!(outcome.event.is_none());
    assert_eq!(role_of(&mailspace, &first), "done", "its own row moved");
    let reported = node(&mailspace, &first);
    assert_eq!(reported.state, "open");
    assert_eq!(reported.readiness, "reported");
    assert_eq!(node(&mailspace, &second).readiness, "blocked");
}

#[test]
fn a_report_does_not_fire_a_bound_needs_join_or_write_its_event() {
    let (_tmp, mailspace) = space();
    let need = send(&mailspace, "need", "lower me", &[]);
    let unit = send(&mailspace, "task", "the unit", &[]);
    mailspace
        .backlog_bind_units(&need, &[unit.clone()])
        .unwrap();

    done(&mailspace, "hand", &unit);

    assert_eq!(node(&mailspace, &need).state, "open");
    assert_eq!(role_of(&mailspace, &need), "needs");
    assert!(mailspace.item_event_state(&need).unwrap().is_none());
    assert!(mailspace.item_event_state(&unit).unwrap().is_none());
}

#[test]
fn the_minds_done_accepts_the_report_and_completes_everything_it_held_back() {
    let (_tmp, mailspace) = space();
    let need = send(&mailspace, "need", "lower me", &[]);
    let unit = send(&mailspace, "task", "the unit", &[]);
    let follower = send(&mailspace, "task", "follows", &[&unit]);
    mailspace
        .backlog_bind_units(&need, &[unit.clone()])
        .unwrap();
    done(&mailspace, "hand", &unit);

    // The acceptor may name the hand's handle, though the hand's row is
    // already in `done`: the event letter is the acceptance.
    let outcome = mailspace
        .move_item_outcome("mind", &unit, "done", None, "task done", None)
        .unwrap();

    assert!(outcome.event.is_some(), "{outcome:?}");
    assert_eq!(node(&mailspace, &unit).state, "done");
    assert_eq!(node(&mailspace, &follower).readiness, "ready");
    assert_eq!(node(&mailspace, &need).state, "done");
    assert_eq!(role_of(&mailspace, &need), "done");
    let event = mailspace.item_event_state(&need).unwrap().unwrap();
    assert_eq!(event.state, "done", "the join emitted the need's event");
}

#[test]
fn the_reporter_reopens_its_own_report() {
    let (_tmp, mailspace) = space();
    let task = send(&mailspace, "task", "unit", &[]);
    done(&mailspace, "hand", &task);

    mailspace
        .move_item("hand", &task, "tasks", None, "task reopen", None)
        .unwrap();

    assert_eq!(role_of(&mailspace, &task), "tasks");
    let view = node(&mailspace, &task);
    assert_eq!(view.state, "open");
    assert_eq!(view.readiness, "ready");
}

#[test]
fn step_lists_reported_work_as_awaiting_acceptance_and_never_dispatches_it() {
    let (_tmp, mailspace) = space();
    let task = send(&mailspace, "task", "unit", &[]);
    assert_eq!(mailspace.step_shadow().unwrap().dispatches.len(), 1);

    done(&mailspace, "hand", &task);

    let manifest = mailspace.step_shadow().unwrap();
    assert!(manifest.dispatches.is_empty(), "{manifest:?}");
    let exception = manifest
        .exceptions
        .iter()
        .find(|e| e.item == task)
        .unwrap_or_else(|| panic!("{manifest:?}"));
    assert_eq!(exception.reason, "awaiting_acceptance");
    assert_eq!(exception.kind, "task");
    assert!(
        exception.detail.contains("reported done by hand"),
        "{exception:?}"
    );
    assert!(exception.detail.contains("'mind'"), "{exception:?}");
    assert!(
        exception
            .detail
            .contains(&format!("vivi task done {task} --for mind")),
        "{exception:?}"
    );
}

#[test]
fn step_lists_reported_active_work_too() {
    let (_tmp, mailspace) = space();
    let task = send(&mailspace, "task", "unit", &[]);
    mailspace
        .graph_activate("backlog", &task, &task, None)
        .unwrap();
    assert_eq!(node(&mailspace, &task).state, "active");

    done(&mailspace, "hand", &task);

    let view = node(&mailspace, &task);
    assert_eq!(view.state, "active");
    assert_eq!(view.readiness, "reported");
    let manifest = mailspace.step_shadow().unwrap();
    assert!(
        manifest
            .exceptions
            .iter()
            .any(|e| e.item == task && e.reason == "awaiting_acceptance"),
        "{manifest:?}"
    );
}

#[test]
fn step_apply_on_a_reported_item_completes_nothing() {
    let (_tmp, mailspace) = space();
    let task = send(&mailspace, "task", "unit", &[]);
    done(&mailspace, "hand", &task);

    let manifest = mailspace.step_apply(&task).unwrap();

    assert!(manifest.decisions.is_empty(), "{manifest:?}");
    assert!(
        manifest
            .exceptions
            .iter()
            .any(|e| e.reason == "awaiting_acceptance"
                && e.detail.contains("done")
                && e.detail.contains("--for mind")),
        "{manifest:?}"
    );
    assert_eq!(node(&mailspace, &task).state, "open");
}

#[test]
fn step_apply_on_an_accepted_item_is_already_settled() {
    let (_tmp, mailspace) = space();
    let task = send(&mailspace, "task", "unit", &[]);
    done(&mailspace, "hand", &task);
    done(&mailspace, "mind", &task);

    let manifest = mailspace.step_apply(&task).unwrap();

    assert_eq!(manifest.decisions.len(), 1, "{manifest:?}");
    assert!(
        manifest.decisions[0].contains("already settled"),
        "{manifest:?}"
    );
}

#[test]
fn the_frontier_names_reported_work_and_keeps_dependents_blocked() {
    let (_tmp, mailspace) = space();
    let first = send(&mailspace, "task", "unit A", &[]);
    let second = send(&mailspace, "task", "unit B", &[&first]);
    done(&mailspace, "hand", &first);

    let frontier = frontier_from_show(&mailspace.graph_show("backlog").unwrap());

    assert_eq!(frontier.reported, [first]);
    assert_eq!(frontier.blocked, [second]);
    assert!(frontier.ready.is_empty(), "{frontier:?}");
}

#[test]
fn activating_a_reported_node_names_the_accept_command() {
    let (_tmp, mailspace) = space();
    let task = send(&mailspace, "task", "unit", &[]);
    done(&mailspace, "hand", &task);

    let error = mailspace
        .graph_activate("backlog", &task, &task, None)
        .unwrap_err()
        .to_string();

    assert!(error.contains("awaits acceptance"), "{error}");
    assert!(
        error.contains(&format!("vivi task done {task} --for mind")),
        "{error}"
    );
}

#[test]
fn graph_complete_stays_the_manual_override() {
    let (_tmp, mailspace) = space();
    let first = send(&mailspace, "task", "unit A", &[]);
    let second = send(&mailspace, "task", "unit B", &[&first]);
    done(&mailspace, "hand", &first);

    mailspace.graph_complete("backlog", &first, None).unwrap();

    assert_eq!(node(&mailspace, &first).state, "done");
    assert_eq!(node(&mailspace, &second).readiness, "ready");
}

#[test]
fn audit_does_not_call_a_report_drift_and_repair_leaves_it() {
    let (_tmp, mailspace) = space();
    let task = send(&mailspace, "task", "unit", &[]);
    done(&mailspace, "hand", &task);

    let report = mailspace.backlog_audit(false).unwrap();
    assert!(report.findings.is_empty(), "{:?}", report.findings);
    let repaired = mailspace.backlog_audit(true).unwrap();
    assert!(repaired.findings.is_empty(), "{:?}", repaired.findings);

    assert_eq!(node(&mailspace, &task).state, "open");
    assert_eq!(node(&mailspace, &task).readiness, "reported");
}

#[test]
fn audit_still_repairs_a_done_folder_under_an_open_node_when_a_done_event_heads_the_item() {
    let (_tmp, mailspace) = space();
    let task = send(&mailspace, "task", "unit", &[]);
    done(&mailspace, "mind", &task);
    let (graph, node_handle) = {
        let storage = mailspace.storage().unwrap();
        let graph = storage.work_graph_by_code("backlog").unwrap().unwrap();
        let row = storage
            .work_graph_nodes(&graph.handle)
            .unwrap()
            .into_iter()
            .find(|n| n.source_id == task)
            .unwrap();
        (graph.handle, row.handle)
    };
    mailspace
        .storage()
        .unwrap()
        .set_work_graph_node_state(&graph, &node_handle, "open", None)
        .unwrap();

    let found = mailspace.backlog_audit(false).unwrap();
    assert_eq!(found.findings.len(), 1, "{:?}", found.findings);
    assert_eq!(found.findings[0].class, "node_state");

    mailspace.backlog_audit(true).unwrap();
    assert_eq!(node(&mailspace, &task).state, "done");
}

#[test]
fn work_completed_before_this_rule_is_not_flagged() {
    let (_tmp, mailspace) = space();
    let task = send(&mailspace, "task", "unit", &[]);
    done(&mailspace, "hand", &task);
    // A binary from before this rule completed the node at report time.
    mailspace.backlog_complete_item(&task).unwrap();

    assert!(mailspace.backlog_audit(false).unwrap().findings.is_empty());
    let manifest = mailspace.step_shadow().unwrap();
    assert!(
        !manifest
            .exceptions
            .iter()
            .any(|e| e.reason == "awaiting_acceptance"),
        "{manifest:?}"
    );
    let frontier = frontier_from_show(&mailspace.graph_show("backlog").unwrap());
    assert!(frontier.reported.is_empty(), "{frontier:?}");
}

/// A task another mailspace authored, delivered to the hand and the mind of
/// this one. Returns `(hand's handle, mind's handle)`.
fn foreign_task(mailspace: &Mailspace) -> (String, String) {
    let letter = format!(
        "From: alice@a1b2c3d4.local\r\nTo: {}, {}\r\nSubject: elsewhere\r\n\
         Message-ID: <elsewhere@a1b2c3d4.local>\r\nX-Vivi-Kind: task\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\ndone_when: lands\r\n",
        mailspace.address_for("hand"),
        mailspace.address_for("mind"),
    );
    let delivery = mailspace.deliver_raw(letter.as_bytes(), "tasks").unwrap();
    let handle_of = |identity: &str| {
        delivery
            .delivered
            .iter()
            .find(|d| d.identity == identity)
            .unwrap()
            .handle
            .clone()
    };
    let (hand, mind) = (handle_of("hand"), handle_of("mind"));
    // Each delivered copy is its own node, as a pull or an import mints them.
    for handle in [&hand, &mind] {
        let message_id = mailspace
            .storage()
            .unwrap()
            .resolve_message_token(handle)
            .unwrap();
        mailspace
            .backlog_attach_letter("task", &message_id, "elsewhere", letter.as_bytes())
            .unwrap();
    }
    (hand, mind)
}

#[test]
fn on_another_mailspaces_item_a_hand_reports_and_the_local_mind_accepts() {
    let (_tmp, mailspace) = space();
    let (hand, mind) = foreign_task(&mailspace);

    let reported = done(&mailspace, "hand", &hand);
    assert!(
        reported.reported && reported.event.is_none(),
        "{reported:?}"
    );
    assert_eq!(node(&mailspace, &hand).state, "open");
    assert_eq!(node(&mailspace, &hand).readiness, "reported");
    let manifest = mailspace.step_shadow().unwrap();
    let exception = manifest
        .exceptions
        .iter()
        .find(|e| e.reason == "awaiting_acceptance")
        .unwrap_or_else(|| panic!("{manifest:?}"));
    assert!(
        exception.detail.contains("a role of kind mind"),
        "{exception:?}"
    );

    // The mind's own close is a local report that is this mailspace's
    // acceptance: no event letter, and the node completes.
    let accepted = done(&mailspace, "mind", &mind);
    assert!(
        accepted.reported && accepted.event.is_none(),
        "{accepted:?}"
    );
    assert_eq!(node(&mailspace, &hand).state, "done");
    assert!(mailspace.item_event_state(&hand).unwrap().is_none());
}

#[test]
fn a_mind_accepts_another_mailspaces_report_by_naming_the_reported_row() {
    let (_tmp, mailspace) = space();
    let (hand, _mind) = foreign_task(&mailspace);
    done(&mailspace, "hand", &hand);

    let accepted = done(&mailspace, "mind", &hand);

    assert!(
        accepted.reported && accepted.event.is_none(),
        "{accepted:?}"
    );
    assert_eq!(node(&mailspace, &hand).state, "done");
}

#[test]
fn a_hand_cannot_accept_another_mailspaces_report_by_naming_it() {
    let (_tmp, mailspace) = space();
    let (hand, mind) = foreign_task(&mailspace);
    done(&mailspace, "hand", &hand);

    let error = mailspace
        .move_item_outcome("auditor", &mind, "done", None, "task done", None)
        .unwrap_err()
        .to_string();

    assert!(error.contains("held by identity"), "{error}");
    assert_eq!(node(&mailspace, &hand).state, "open");
}

#[test]
fn with_no_author_role_and_no_mind_step_names_the_manual_path() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("hand").unwrap();
    let letter = format!(
        "From: alice@a1b2c3d4.local\r\nTo: {}\r\nSubject: elsewhere\r\n\
         Message-ID: <elsewhere@a1b2c3d4.local>\r\nX-Vivi-Kind: task\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\ndone_when: lands\r\n",
        mailspace.address_for("hand"),
    );
    let handle = mailspace
        .deliver_raw(letter.as_bytes(), "tasks")
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    let message_id = mailspace
        .storage()
        .unwrap()
        .resolve_message_token(&handle)
        .unwrap();
    mailspace
        .backlog_attach_letter("task", &message_id, "elsewhere", letter.as_bytes())
        .unwrap();
    done(&mailspace, "hand", &handle);

    let manifest = mailspace.step_shadow().unwrap();

    let exception = manifest
        .exceptions
        .iter()
        .find(|e| e.reason == "awaiting_acceptance")
        .unwrap_or_else(|| panic!("{manifest:?}"));
    assert!(
        exception.detail.contains("no local role may accept"),
        "{exception:?}"
    );
    assert!(
        exception
            .detail
            .contains(&format!("vivi graph complete {handle}")),
        "{exception:?}"
    );
}
