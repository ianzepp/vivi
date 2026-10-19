//! The pull side of dependency edges and unknown or retired roles: whatever
//! route files a letter, the receiver's graph, `step` and
//! `task list --blocked` agree.

use crate::mailspace::{GraphNodeView, RoleUpdate, SourceTaskRequest};
use crate::sync::fake_server::{LETTER_DATE, World, wire_id};
use crate::sync::fetch::fetch_handles;
use crate::sync::pull::{PullPlan, pull_with};
use crate::sync::refs::follow_refs;

fn put(w: &World, n: u64, kind: &str, to: &str, subject: &str, deps: &[u64]) -> String {
    let headers: String = deps
        .iter()
        .map(|dep| format!("X-Vivi-Depends-On: {dep:08x}\r\n"))
        .collect();
    w.put_full(
        &wire_id(n, 0),
        kind,
        to,
        subject,
        "done_when: it is done\r\n",
        LETTER_DATE,
        &headers,
    )
}

fn node(w: &World, subject: &str) -> GraphNodeView {
    let show = w.bob.mailspace.graph_show("backlog").unwrap();
    show.nodes
        .into_iter()
        .find(|node| node.label == subject)
        .unwrap_or_else(|| panic!("no node for {subject}"))
}

fn has_node(w: &World, subject: &str) -> bool {
    w.bob
        .mailspace
        .graph_show("backlog")
        .map(|show| show.nodes.iter().any(|node| node.label == subject))
        .unwrap_or(false)
}

fn blocked_subjects(w: &World, role: &str) -> Vec<String> {
    w.bob
        .mailspace
        .list_tasks_with_deps(role, true, None)
        .unwrap()
        .into_iter()
        .map(|(view, _)| view.subject)
        .collect()
}

fn dispatched(w: &World) -> Vec<String> {
    let manifest = w.bob.mailspace.step_shadow().unwrap();
    let show = w.bob.mailspace.graph_show("backlog").unwrap();
    manifest
        .dispatches
        .iter()
        .filter_map(|d| {
            show.nodes
                .iter()
                .find(|n| n.source_id == d.item)
                .map(|n| n.label.clone())
        })
        .collect()
}

#[test]
fn a_pulled_letter_gets_the_edges_its_header_names() {
    let w = World::new();
    let hand = w.bob.address("hand");
    put(&w, 0xa1, "need", &hand, "first", &[]);
    put(&w, 0xb2, "task", &hand, "second", &[0xa1]);

    let report = w.bob.pull(&w.bob_client);

    assert_eq!(report.filed.values().sum::<usize>(), 2);
    let second = node(&w, "second");
    assert_eq!(second.readiness, "blocked");
    assert_eq!(second.blocked_by.len(), 1, "{second:?}");
    assert_eq!(node(&w, "first").readiness, "ready");
    assert_eq!(blocked_subjects(&w, "hand"), ["second"], "list agrees");
    assert_eq!(
        dispatched(&w),
        ["first"],
        "step does not dispatch the dependent"
    );
}

#[test]
fn a_dependency_that_arrives_in_a_later_pull_makes_the_edge_then() {
    let w = World::new();
    let hand = w.bob.address("hand");
    put(&w, 0xb2, "task", &hand, "second", &[0xa1]);
    w.bob.pull(&w.bob_client);
    let waiting = node(&w, "second");
    assert_eq!(
        waiting.readiness, "blocked",
        "the unheld dependency blocks it"
    );
    assert!(
        waiting.blocked_by.contains(&"000000a1".to_string()),
        "{waiting:?}"
    );
    assert_eq!(blocked_subjects(&w, "hand"), ["second"]);
    assert!(
        dispatched(&w).is_empty(),
        "it waits until its dependency arrives"
    );

    put(&w, 0xa1, "need", &hand, "first", &[]);
    w.bob.pull(&w.bob_client);

    assert_eq!(node(&w, "second").readiness, "blocked");
    assert_eq!(blocked_subjects(&w, "hand"), ["second"]);
    assert_eq!(
        dispatched(&w),
        ["first"],
        "the dependency, not the dependent"
    );

    // Once the dependency arrives and is done, the dependent becomes ready.
    let first = node(&w, "first").source_id;
    w.bob.mailspace.backlog_complete_item(&first).unwrap();
    assert_eq!(node(&w, "second").readiness, "ready");
    assert_eq!(dispatched(&w), ["second"]);
}

#[test]
fn a_dependency_that_arrives_by_fetch_makes_the_edge() {
    let w = World::new();
    let hand = w.bob.address("hand");
    put(&w, 0xb2, "task", &hand, "second", &[0xa1]);
    w.bob.pull(&w.bob_client);
    // The dependency is on the server but behind bob's cursor: fetched, not
    // pulled.
    put(&w, 0xa1, "need", &hand, "first", &[]);
    assert_eq!(
        node(&w, "second").readiness,
        "blocked",
        "the fetch has nothing to unblock until the dependency lands"
    );

    let report = fetch_handles(
        &w.bob.mailspace,
        &w.bob.settings,
        &w.bob_client,
        &[format!("{:08x}", 0xa1)],
    )
    .unwrap();

    assert_eq!(report.pull.filed.values().sum::<usize>(), 1);
    assert_eq!(node(&w, "second").readiness, "blocked");
    assert_eq!(blocked_subjects(&w, "hand"), ["second"]);
}

#[test]
fn a_dependency_that_arrives_by_reference_follow_makes_the_edge() {
    let w = World::new();
    let hand = w.bob.address("hand");
    // Bob's window starts after the dependency; the follow fetches it.
    w.at("2026-09-01T00:00:00Z");
    put(&w, 0xa1, "need", &hand, "first", &[]);
    w.at("2026-10-10T00:00:00Z");
    put(&w, 0xb2, "task", &hand, "second", &[0xa1]);
    let plan = PullPlan {
        window: Some("2026-10-05T00:00:00Z"),
        resync: false,
    };
    let mut report = pull_with(
        &w.bob.mailspace,
        &w.bob.settings,
        &w.bob_client,
        &plan,
        &mut |_| {},
    )
    .unwrap();
    assert!(has_node(&w, "second"));
    assert!(!has_node(&w, "first"), "outside the window");
    assert_eq!(
        node(&w, "second").readiness,
        "blocked",
        "the dependency is outside the window, not satisfied"
    );
    assert!(dispatched(&w).is_empty());
    let manifest = w.bob.mailspace.step_shadow().unwrap();
    let waiting: Vec<_> = manifest
        .exceptions
        .iter()
        .filter(|exc| exc.reason == "awaiting_dependency")
        .collect();
    assert_eq!(waiting.len(), 1, "{:?}", manifest.exceptions);
    assert_eq!(waiting[0].item, node(&w, "second").source_id);

    follow_refs(
        &w.bob.mailspace,
        &w.bob.settings,
        &w.bob_client,
        &mut report,
        1,
    )
    .unwrap();

    assert!(has_node(&w, "first"));
    assert_eq!(node(&w, "second").readiness, "blocked");
    assert_eq!(blocked_subjects(&w, "hand"), ["second"]);
}

#[test]
fn a_letter_held_for_another_mailspace_orders_nothing() {
    let w = World::new();
    put(&w, 0xa1, "need", &w.bob.address("hand"), "ours", &[]);
    put(&w, 0xb2, "task", "hand@a1b2c3d4.local", "theirs", &[0xa1]);

    w.bob.pull(&w.bob_client);

    assert!(has_node(&w, "ours"));
    assert!(
        !has_node(&w, "theirs"),
        "foreign work mints no node, so no edge"
    );
    let show = w.bob.mailspace.graph_show("backlog").unwrap();
    assert!(show.edges.is_empty(), "{:?}", show.edges);
}

#[test]
fn a_replay_of_held_letters_changes_nothing() {
    let w = World::new();
    let hand = w.bob.address("hand");
    put(&w, 0xa1, "need", &hand, "first", &[]);
    put(&w, 0xb2, "task", &hand, "second", &[0xa1]);
    w.bob.pull(&w.bob_client);
    let before = w.bob.mailspace.graph_show("backlog").unwrap().edges;

    let plan = PullPlan {
        window: None,
        resync: true,
    };
    pull_with(
        &w.bob.mailspace,
        &w.bob.settings,
        &w.bob_client,
        &plan,
        &mut |_| {},
    )
    .unwrap();

    assert_eq!(w.bob.mailspace.graph_show("backlog").unwrap().edges, before);
    assert!(
        w.bob
            .mailspace
            .backlog_audit(false)
            .unwrap()
            .findings
            .is_empty()
    );
}

#[test]
fn filing_rebuilds_the_need_bind_from_the_parent_header() {
    let w = World::new();
    let hand = w.bob.address("hand");
    // Alice sends the need to bob's hand and pushes it; bob pulls it.
    let need = w.alice.send_need("mind", &hand, "the need");
    w.alice.push(&w.alice.client(&w.server));
    w.bob.pull(&w.bob_client);
    assert_eq!(node(&w, "the need").state, "open", "the need is held work");

    // Alice lowers the need into a unit task addressed to bob's hand. The
    // bind is local to alice: the letter carries only the parent header.
    w.alice
        .mailspace
        .task_from_source(SourceTaskRequest {
            source_handle: need,
            actor: "mind".into(),
            to: vec![hand.clone()],
            cc: Vec::new(),
            subject: "the unit".into(),
            body: "do the first part\r\n".into(),
        })
        .unwrap();
    w.alice.push(&w.alice.client(&w.server));
    w.bob.pull(&w.bob_client);

    let need_node = node(&w, "the need");
    assert_eq!(need_node.state, "open");
    let unit = node(&w, "the unit");
    assert_eq!(
        unit.subgraph.as_deref(),
        Some(need_node.source_id.as_str()),
        "filing derives the bind from X-Vivi-Parent"
    );

    // Pulling the same letters again writes no second bind.
    let before = w.bob.mailspace.graph_show("backlog").unwrap().nodes;
    w.bob.pull(&w.bob_client);
    assert_eq!(w.bob.mailspace.graph_show("backlog").unwrap().nodes, before);
    assert!(
        w.bob
            .mailspace
            .backlog_audit(true)
            .unwrap()
            .findings
            .is_empty()
    );
}

#[test]
fn a_letter_for_an_unknown_role_is_counted_held_and_not_dispatched() {
    let mut w = World::new();
    let ghost = format!("ghost@{}", w.bob.mailspace.address_domain());
    put(&w, 0xa1, "task", &ghost, "for a ghost", &[]);
    put(&w, 0xb2, "task", &w.bob.address("hand"), "for hand", &[]);

    let report = w.bob.pull(&w.bob_client);

    assert!(report.failure().is_none(), "held, not refused");
    assert_eq!(report.unknown_roles.get("ghost"), Some(&1));
    let summary = report.summary();
    assert!(
        summary.contains("1 held for unknown role(s): ghost"),
        "{summary}"
    );
    assert!(
        report
            .warnings
            .contains(&"filed for unknown role ghost".to_string())
    );
    assert!(!has_node(&w, "for a ghost"), "no node: not dispatchable");
    assert_eq!(dispatched(&w), ["for hand"]);
    assert_eq!(
        w.bob.mailspace.held_for_unknown_roles().unwrap(),
        [("ghost".to_string(), 1)]
    );

    // A second pull of nothing new reports nothing held.
    let again = w.bob.pull(&w.bob_client);
    assert!(again.unknown_roles.is_empty());
    assert!(!again.summary().contains("held for unknown"));

    // Adding the role adopts the letter and step dispatches it.
    w.bob.mailspace.add_identity("ghost").unwrap();
    assert_eq!(w.bob.mailspace.adopt_held_letters("ghost").unwrap(), 1);
    assert!(dispatched(&w).contains(&"for a ghost".to_string()));
}

#[test]
fn a_fetch_summary_names_the_roles_it_held_letters_for() {
    let w = World::new();
    let ghost = format!("ghost@{}", w.bob.mailspace.address_domain());
    put(&w, 0xa1, "need", &ghost, "for a ghost", &[]);

    let report = fetch_handles(
        &w.bob.mailspace,
        &w.bob.settings,
        &w.bob_client,
        &[format!("{:08x}", 0xa1)],
    )
    .unwrap();

    let summary = report.summary();
    assert!(
        summary.contains("1 held for unknown role(s): ghost"),
        "{summary}"
    );
}

#[test]
fn a_retired_role_warns_once_per_pull_and_still_receives() {
    let mut w = World::new();
    w.bob
        .mailspace
        .set_role(
            "hand",
            RoleUpdate {
                status: Some("retired".into()),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    let hand = w.bob.address("hand");
    put(&w, 0xa1, "task", &hand, "one", &[]);
    put(&w, 0xb2, "task", &hand, "two", &[]);

    let report = w.bob.pull(&w.bob_client);

    let retired: Vec<_> = report
        .warnings
        .iter()
        .filter(|w| w.contains("is retired"))
        .collect();
    assert_eq!(retired, ["role hand is retired"], "{:?}", report.warnings);
    assert_eq!(report.filed.get("tasks"), Some(&2), "filing is unchanged");
    assert!(has_node(&w, "one") && has_node(&w, "two"));
    assert!(report.failure().is_none());
}
