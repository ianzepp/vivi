use super::*;
use crate::storage::{BACKLOG_GRAPH_CODE, Storage};

fn storage() -> (tempfile::TempDir, Storage) {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::open_mailspace(tmp.path()).unwrap();
    (tmp, storage)
}

fn node(source_id: &str) -> BacklogNodeInput {
    BacklogNodeInput {
        source_id: source_id.into(),
        label: format!("label {source_id}"),
        state: "open".into(),
        kind: "need".into(),
    }
}

fn edge(from: &str, to: &str) -> WorkGraphEdgeInput {
    WorkGraphEdgeInput {
        from_source_id: from.into(),
        to_source_id: to.into(),
        label: None,
        style: "solid".into(),
    }
}

fn mint(storage: &Storage, nodes: &[&str], edges: &[(&str, &str)]) -> BacklogMintCommit {
    storage
        .mint_backlog(&BacklogMintInput {
            nodes: nodes.iter().map(|id| node(id)).collect(),
            edges: edges.iter().map(|(a, b)| edge(a, b)).collect(),
        })
        .unwrap()
}

fn event_count(storage: &Storage, graph: &str) -> usize {
    storage
        .conn
        .borrow()
        .query_row(
            "SELECT COUNT(*) FROM work_graph_events
             WHERE graph_handle = ?1 AND event_type = 'backlog_attached'",
            params![graph],
            |row| row.get::<_, i64>(0),
        )
        .unwrap() as usize
}

#[test]
fn a_first_mint_creates_the_graph_with_its_nodes_and_edges() {
    let (_tmp, storage) = storage();
    let commit = mint(&storage, &["a1", "b2"], &[("a1", "b2")]);

    assert!(commit.created_graph);
    assert_eq!(commit.minted_nodes.len(), 2);
    assert_eq!(commit.minted_edges, 1);
    assert_eq!(
        storage
            .work_graph_nodes(&commit.graph_handle)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        storage
            .work_graph_edges(&commit.graph_handle)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(event_count(&storage, &commit.graph_handle), 2);
}

#[test]
fn minting_the_same_input_again_adds_nothing() {
    let (_tmp, storage) = storage();
    let first = mint(&storage, &["a1", "b2"], &[("a1", "b2")]);
    let updated: String = storage
        .work_graph_by_code(BACKLOG_GRAPH_CODE)
        .unwrap()
        .unwrap()
        .updated_at;

    let again = mint(&storage, &["a1", "b2"], &[("a1", "b2")]);

    assert!(!again.created_graph);
    assert!(again.minted_nodes.is_empty());
    assert_eq!(again.minted_edges, 0);
    assert_eq!(
        storage.work_graph_nodes(&first.graph_handle).unwrap().len(),
        2
    );
    assert_eq!(
        storage.work_graph_edges(&first.graph_handle).unwrap().len(),
        1
    );
    assert_eq!(event_count(&storage, &first.graph_handle), 2);
    let after = storage
        .work_graph_by_code(BACKLOG_GRAPH_CODE)
        .unwrap()
        .unwrap();
    assert_eq!(
        after.updated_at, updated,
        "a no-op mint must not touch the graph"
    );
}

#[test]
fn a_mint_that_overlaps_adds_only_what_is_new() {
    let (_tmp, storage) = storage();
    let first = mint(&storage, &["a1", "b2"], &[("a1", "b2")]);

    let next = mint(&storage, &["b2", "c3"], &[("a1", "b2"), ("b2", "c3")]);

    assert_eq!(
        next.minted_nodes,
        vec![node_handle_for(&first.graph_handle, "c3")]
    );
    assert_eq!(next.minted_edges, 1);
    assert_eq!(
        storage.work_graph_nodes(&first.graph_handle).unwrap().len(),
        3
    );
    assert_eq!(
        storage.work_graph_edges(&first.graph_handle).unwrap().len(),
        2
    );
    assert_eq!(event_count(&storage, &first.graph_handle), 3);
}

#[test]
fn an_existing_node_keeps_its_state_and_label() {
    let (_tmp, storage) = storage();
    let first = mint(&storage, &["a1"], &[]);
    let handle = node_handle_for(&first.graph_handle, "a1");
    storage
        .set_work_graph_node_state(&first.graph_handle, &handle, "done", None)
        .unwrap();

    let mut changed = node("a1");
    changed.label = "a different label".into();
    changed.state = "open".into();
    let again = storage
        .mint_backlog(&BacklogMintInput {
            nodes: vec![changed],
            edges: Vec::new(),
        })
        .unwrap();

    assert!(again.minted_nodes.is_empty());
    let nodes = storage.work_graph_nodes(&first.graph_handle).unwrap();
    assert_eq!(nodes[0].state, "done");
    assert_eq!(nodes[0].label, "label a1");
}
