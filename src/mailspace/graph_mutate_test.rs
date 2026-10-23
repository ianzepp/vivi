use super::*;
use tempfile::tempdir;

fn base() -> &'static str {
    "flowchart LR\na[\"A\"]\nb[\"B\"]\na --> b\n"
}

#[test]
fn activate_pins_task_content_in_binding_event() {
    let dir = tempdir().unwrap();
    let mut ms = Mailspace::init(Some(dir.path())).unwrap();
    ms.add_identity("ceo").unwrap();
    ms.add_identity("cto").unwrap();
    ms.graph_import("demo", base(), false).unwrap();
    let task = ms
        .send(crate::mailspace::SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "unit".into(),
            body: crate::letter::fixture::body_for("task", "lands"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;

    ms.graph_activate("demo", "a", &task, None).unwrap();

    let content = ms.content_hash_of(&task).unwrap();
    let storage = ms.storage().unwrap();
    let events = storage.list_work_graph_events_after(0).unwrap();
    let bound = events
        .iter()
        .find(|e| e.event_type == "attempt_bound")
        .expect("attempt_bound event");
    let note = bound.note.as_deref().unwrap_or_default();
    assert!(note.contains(&format!("task={task}")), "{note}");
    assert!(note.contains(&format!("content={content}")), "{note}");
}

#[test]
fn complete_unlocks_successor() {
    let dir = tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    ms.graph_import("demo", base(), false).unwrap();
    let before = ms.graph_show("demo").unwrap();
    assert_eq!(before.ready.len(), 1);
    assert_eq!(before.ready[0].source_id, "a");
    let after = ms.graph_complete("demo", "a", Some("done")).unwrap();
    assert!(after.ready.iter().any(|n| n.source_id == "b"));
    let a = after.nodes.iter().find(|n| n.source_id == "a").unwrap();
    assert_eq!(a.state, "done");
}

#[test]
fn apply_adds_successor_to_done() {
    let dir = tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    ms.graph_import("demo", base(), false).unwrap();
    ms.graph_complete("demo", "a", None).unwrap();
    ms.graph_complete("demo", "b", None).unwrap();
    let expanded = "flowchart LR\na[\"A\"]\nb[\"B\"]\nc[\"C\"]\na --> b\nb --> c\n";
    let report = ms.graph_apply("demo", expanded, false).unwrap();
    assert!(report.nodes_added.contains(&"c".to_string()));
    assert_eq!(report.revision, 2);
    let show = ms.graph_show("demo").unwrap();
    assert!(show.ready.iter().any(|n| n.source_id == "c"));
}

#[test]
fn apply_rejects_prereq_change_on_done() {
    let dir = tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    ms.graph_import("demo", base(), false).unwrap();
    ms.graph_complete("demo", "a", None).unwrap();
    ms.graph_complete("demo", "b", None).unwrap();
    let bad = "flowchart LR\na[\"A\"]\nb[\"B\"]\nx[\"X\"]\nx --> b\n";
    let err = ms.graph_apply("demo", bad, false).unwrap_err().to_string();
    assert!(
        err.contains("prerequisite") || err.contains("remove"),
        "{err}"
    );
}

#[test]
fn export_round_trip_topology() {
    let dir = tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    ms.graph_import("demo", base(), false).unwrap();
    let mermaid = ms.graph_export_mermaid("demo", false).unwrap();
    let again = ms.graph_apply("demo", &mermaid, false).unwrap();
    assert!(again.idempotent || again.revision >= 1);
    let show = ms.graph_show("demo").unwrap();
    assert_eq!(show.nodes.len(), 2);
    assert_eq!(show.edges.len(), 1);
}

#[test]
fn activate_refuses_blocked_node() {
    let dir = tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    ms.graph_import("demo", base(), false).unwrap();
    let err = ms
        .graph_activate("demo", "b", "deadbeef", None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("blocked"), "{err}");
}

#[test]
fn activate_refuses_gate_kinds() {
    let dir = tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    ms.graph_import("demo", "flowchart LR\ndec{\"ruling\"}\n", false)
        .unwrap();
    let err = ms
        .graph_activate("demo", "dec", "deadbeef", None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("decision"), "{err}");
    assert!(err.contains("graph complete"), "{err}");
}

#[test]
fn complete_resolves_gate_nodes() {
    let dir = tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    ms.graph_import(
        "demo",
        "flowchart LR\ndec{\"ruling\"}\nwork[\"unit\"]\ndec --> work\n",
        false,
    )
    .unwrap();
    let after = ms
        .graph_complete("demo", "dec", Some("ruling: ship"))
        .unwrap();
    assert!(after.ready.iter().any(|n| n.source_id == "work"));
    let dec = after.nodes.iter().find(|n| n.source_id == "dec").unwrap();
    assert_eq!(dec.state, "done");
}

#[test]
fn export_round_trip_preserves_kinds_and_couplings() {
    let dir = tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    let src = "flowchart LR\na[\"A\"]\nd{\"D\"}\np[\"P\"]:::parked\na --> d\nd -.-> p\n";
    ms.graph_import("demo", src, false).unwrap();
    let mermaid = ms.graph_export_mermaid("demo", false).unwrap();
    let parsed = parse_flowchart(&mermaid).unwrap();
    assert_eq!(
        parsed
            .nodes
            .iter()
            .find(|n| n.source_id == "d")
            .unwrap()
            .kind,
        "decision"
    );
    assert_eq!(
        parsed
            .nodes
            .iter()
            .find(|n| n.source_id == "p")
            .unwrap()
            .kind,
        "parked"
    );
    assert_eq!(
        parsed
            .edges
            .iter()
            .find(|e| e.from == "d" && e.to == "p")
            .unwrap()
            .style,
        "dotted"
    );
    let report = ms.graph_apply("demo", &mermaid, false).unwrap();
    assert!(report.nodes_added.is_empty());
    assert!(report.nodes_updated.is_empty());
}

#[test]
fn apply_flips_edge_style_between_solid_and_dotted() {
    let dir = tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    ms.graph_import("demo", base(), false).unwrap();
    let dotted = "flowchart LR\na[\"A\"]\nb[\"B\"]\na -.-> b\n";
    let report = ms.graph_apply("demo", dotted, false).unwrap();
    assert_eq!(report.edges_added, 1);
    assert_eq!(report.edges_removed, 1);
    let show = ms.graph_show("demo").unwrap();
    assert!(show.ready.iter().any(|n| n.source_id == "b"));
}

#[test]
fn node_add_supports_gate_kinds() {
    let dir = tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    ms.graph_import("demo", base(), false).unwrap();
    ms.graph_node_add("demo", "gate1", Some("pending ruling"), Some("decision"))
        .unwrap();
    let show = ms.graph_show("demo").unwrap();
    let gate = show.nodes.iter().find(|n| n.source_id == "gate1").unwrap();
    assert_eq!(gate.kind, "decision");
    let err = ms
        .graph_node_add("demo", "bad", None, Some("urgent"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("invalid node kind"), "{err}");
}
