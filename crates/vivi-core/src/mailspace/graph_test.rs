use super::*;
use tempfile::tempdir;

fn sample() -> &'static str {
    r#"
flowchart LR
  verify["reverify"]
  accept["accept"]
  u2["U2"]
  u3["U3"]
  verify --> accept
  accept --> u2
  accept --> u3
"#
}

#[test]
fn import_and_show_ready_roots() {
    let dir = tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    let report = ms.graph_import("demo", sample(), false).unwrap();
    assert!(report.created);
    assert_eq!(report.node_count, 4);
    assert_eq!(report.edge_count, 3);
    assert_eq!(report.ready.len(), 1);
    assert_eq!(report.ready[0].source_id, "verify");

    let show = ms.graph_show("demo").unwrap();
    assert_eq!(show.ready.len(), 1);
    assert_eq!(show.blocked.len(), 3);
    let accept = show.nodes.iter().find(|n| n.source_id == "accept").unwrap();
    assert_eq!(accept.blocked_by, vec!["verify".to_string()]);
}

#[test]
fn check_only_writes_nothing() {
    let dir = tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    let report = ms.graph_import("demo", sample(), true).unwrap();
    assert!(report.check_only);
    assert!(ms.graph_show("demo").is_err());
}

#[test]
fn idempotent_reimport() {
    let dir = tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    let first = ms.graph_import("demo", sample(), false).unwrap();
    let second = ms.graph_import("demo", sample(), false).unwrap();
    assert!(second.idempotent);
    assert_eq!(first.graph_handle, second.graph_handle);
    assert_eq!(second.revision, 1);
}

#[test]
fn conflict_on_different_source() {
    let dir = tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    ms.graph_import("demo", sample(), false).unwrap();
    let err = ms
        .graph_import("demo", "flowchart TD\na --> b\n", false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("already exists"), "{err}");
}

#[test]
fn frontier_lists_gates_separately_from_ready_work() {
    let dir = tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    let src =
        "flowchart LR\nwork[\"unit\"]\ndec40{\"operator ruling\"}\nlater[\"later\"]:::parked\n";
    ms.graph_import("demo", src, false).unwrap();
    let frontier = frontier_from_show(&ms.graph_show("demo").unwrap());
    assert_eq!(frontier.ready, vec!["work".to_string()]);
    assert_eq!(frontier.gates.len(), 2);
    assert!(
        frontier
            .gates
            .iter()
            .any(|g| g.source_id == "dec40" && g.kind == "decision")
    );
    assert!(
        frontier
            .gates
            .iter()
            .any(|g| g.source_id == "later" && g.kind == "parked")
    );
}

#[test]
fn dotted_couplings_never_block_readiness() {
    let dir = tempdir().unwrap();
    let ms = Mailspace::init(Some(dir.path())).unwrap();
    let src = "flowchart LR\nopen[\"not done\"]\nwork[\"unit\"]\nopen -.-> work\n";
    ms.graph_import("demo", src, false).unwrap();
    let show = ms.graph_show("demo").unwrap();
    let work = show.nodes.iter().find(|n| n.source_id == "work").unwrap();
    assert_eq!(work.readiness, "ready");
    assert!(work.blocked_by.is_empty());
}
