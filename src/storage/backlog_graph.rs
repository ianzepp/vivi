//! Backlog citizenship: transactional minting of needs/wants into the
//! `backlog` work graph without Mermaid re-export round-trips.

use std::collections::HashSet;

use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, params};

use super::Storage;
use super::graph::{
    WorkGraphNodeRow, insert_edges, insert_work_graph, map_node_row, node_handle_for,
};
use super::{VivariumError, WorkGraphEdgeInput, WorkGraphImportInput, sha256_hex};

/// Project-unique code of the per-mailspace backlog graph.
pub const BACKLOG_GRAPH_CODE: &str = "backlog";

/// One backlog node to mint, carrying an explicit initial state so
/// retroactive dependencies of already-done items unlock immediately.
#[derive(Debug, Clone)]
pub struct BacklogNodeInput {
    pub source_id: String,
    pub label: String,
    pub state: String,
    pub kind: String,
}

/// Mint request: nodes plus prerequisite→dependent edges between them.
#[derive(Debug, Clone)]
pub struct BacklogMintInput {
    pub nodes: Vec<BacklogNodeInput>,
    pub edges: Vec<WorkGraphEdgeInput>,
}

/// Result of a backlog mint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklogMintCommit {
    pub graph_handle: String,
    pub created_graph: bool,
    pub minted_nodes: Vec<String>,
    pub minted_edges: usize,
}

impl Storage {
    /// Mint nodes and edges into the `backlog` graph in one transaction,
    /// creating the graph (revision 1 with synthesized Mermaid evidence) on
    /// first use. Existing nodes and edges are skipped, so the call is
    /// idempotent.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the transaction fails.
    pub fn mint_backlog(
        &mut self,
        input: &BacklogMintInput,
    ) -> Result<BacklogMintCommit, VivariumError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|e| VivariumError::Other(format!("failed to begin backlog mint: {e}")))?;
        let commit = mint_backlog_tx(&tx, input)?;
        tx.commit()
            .map_err(|e| VivariumError::Other(format!("failed to commit backlog mint: {e}")))?;
        Ok(commit)
    }

    /// Bind unit nodes to a parent need node by setting each unit's
    /// `subgraph` to the parent's source id, in one transaction with one
    /// `unit_bound` event per unit. Each event carries the controlling
    /// bind — `at` (Unix microseconds) and the `bound` letter's
    /// `Message-ID` (`letter`, empty when the bind wrote no letter) — so
    /// every member that applies the same letters holds the same floor,
    /// never a receiver clock.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if any update fails.
    pub fn bind_backlog_units(
        &mut self,
        graph_handle: &str,
        parent_source_id: &str,
        unit_handles: &[String],
        at: i64,
        letter: &str,
    ) -> Result<(), VivariumError> {
        let now = Utc::now().to_rfc3339();
        let occurred = occurred_at(at);
        let tx = self
            .conn
            .transaction()
            .map_err(|e| VivariumError::Other(format!("failed to begin backlog bind: {e}")))?;
        for unit_handle in unit_handles {
            let changed = tx
                .execute(
                    "UPDATE work_graph_nodes SET subgraph = ?1, updated_at = ?2
                     WHERE handle = ?3 AND graph_handle = ?4",
                    params![parent_source_id, now, unit_handle, graph_handle],
                )
                .map_err(|e| VivariumError::Other(format!("failed to bind backlog unit: {e}")))?;
            if changed == 0 {
                return Err(VivariumError::Message(format!(
                    "graph node not found: {unit_handle}"
                )));
            }
            tx.execute(
                "INSERT INTO work_graph_events
                   (graph_handle, occurred_at, event_type, node_handle, note)
                 VALUES (?1, ?2, 'unit_bound', ?3, ?4)",
                params![
                    graph_handle,
                    occurred,
                    unit_handle,
                    bind_note(parent_source_id, letter)
                ],
            )
            .map_err(|e| VivariumError::Other(format!("failed to insert bind event: {e}")))?;
        }
        tx.execute(
            "UPDATE work_graphs SET updated_at = ?1 WHERE handle = ?2",
            params![now, graph_handle],
        )
        .map_err(|e| VivariumError::Other(format!("failed to touch backlog graph: {e}")))?;
        tx.commit()
            .map_err(|e| VivariumError::Other(format!("failed to commit backlog bind: {e}")))?;
        Ok(())
    }

    /// Load nodes whose `subgraph` names the given parent source id.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the query fails.
    pub fn message_ids_by_content(&self, content_id: &str) -> Result<Vec<String>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT message_id FROM messages
                 WHERE content_id = ?1 AND deleted_at IS NULL
                   AND local_role IN ('tasks', 'needs', 'wants', 'done')
                 ORDER BY message_id",
            )
            .map_err(|e| {
                VivariumError::Other(format!("failed to prepare content messages: {e}"))
            })?;
        let rows = stmt
            .query_map(params![content_id], |row| row.get::<_, String>(0))
            .map_err(|e| VivariumError::Other(format!("failed to query content messages: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| {
                VivariumError::Other(format!("failed to read content message: {e}"))
            })?);
        }
        Ok(out)
    }

    /// Load nodes whose `subgraph` names the given parent source id.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the query fails.
    pub fn work_graph_nodes_by_subgraph(
        &self,
        graph_handle: &str,
        parent_source_id: &str,
    ) -> Result<Vec<WorkGraphNodeRow>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT handle, graph_handle, source_id, label, state, subgraph, kind,
                        created_at, updated_at
                 FROM work_graph_nodes WHERE graph_handle = ?1 AND subgraph = ?2
                 ORDER BY source_id",
            )
            .map_err(|e| VivariumError::Other(format!("failed to prepare subgraph nodes: {e}")))?;
        let rows = stmt
            .query_map(params![graph_handle, parent_source_id], map_node_row)
            .map_err(|e| VivariumError::Other(format!("failed to query subgraph nodes: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(
                row.map_err(|e| {
                    VivariumError::Other(format!("failed to read subgraph node: {e}"))
                })?,
            );
        }
        Ok(out)
    }
}

/// The controlling instant as a `unit_bound` event's `occurred_at` stamp,
/// in RFC 3339 with microseconds so it reads back to the same instant.
fn occurred_at(at: i64) -> String {
    chrono::DateTime::from_timestamp_micros(at)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// The `unit_bound` note: the parent the units landed on, and the `bound`
/// letter that moved them when one exists.
fn bind_note(parent_source_id: &str, letter: &str) -> String {
    if letter.is_empty() {
        format!("parent={parent_source_id}")
    } else {
        format!("parent={parent_source_id} letter=<{letter}>")
    }
}

fn mint_backlog_tx(
    tx: &Transaction<'_>,
    input: &BacklogMintInput,
) -> Result<BacklogMintCommit, VivariumError> {
    let now = Utc::now().to_rfc3339();
    let (graph_handle, created_graph) = ensure_backlog_graph(tx, input)?;
    let existing_nodes = existing_node_ids(tx, &graph_handle)?;
    let mut minted_nodes = Vec::new();
    for node in &input.nodes {
        if existing_nodes.contains(&node.source_id) {
            continue;
        }
        insert_backlog_node(tx, &graph_handle, node, &now)?;
        minted_nodes.push(node_handle_for(&graph_handle, &node.source_id));
    }
    let existing_edges = existing_edge_pairs(tx, &graph_handle)?;
    let new_edges: Vec<WorkGraphEdgeInput> = input
        .edges
        .iter()
        .filter(|edge| {
            let from = node_handle_for(&graph_handle, &edge.from_source_id);
            let to = node_handle_for(&graph_handle, &edge.to_source_id);
            !existing_edges.contains(&(from, to))
        })
        .cloned()
        .collect();
    let minted_edges = new_edges.len();
    insert_edges(tx, &graph_handle, &new_edges, &now)?;
    if created_graph || !minted_nodes.is_empty() || minted_edges > 0 {
        tx.execute(
            "UPDATE work_graphs SET updated_at = ?1 WHERE handle = ?2",
            params![now, graph_handle],
        )
        .map_err(|e| VivariumError::Other(format!("failed to touch backlog graph: {e}")))?;
    }
    Ok(BacklogMintCommit {
        graph_handle,
        created_graph,
        minted_nodes,
        minted_edges,
    })
}

fn ensure_backlog_graph(
    tx: &Transaction<'_>,
    input: &BacklogMintInput,
) -> Result<(String, bool), VivariumError> {
    if let Some(handle) = existing_backlog_handle(tx)? {
        return Ok((handle, false));
    }
    let source = synthesize_backlog_mermaid(&input.nodes, &input.edges);
    let commit = insert_work_graph(
        tx,
        &WorkGraphImportInput {
            code: BACKLOG_GRAPH_CODE.to_string(),
            mermaid_source: source.clone(),
            content_hash: sha256_hex(source.as_bytes()),
            nodes: Vec::new(),
            edges: Vec::new(),
        },
    )?;
    Ok((commit.graph.handle, true))
}

fn existing_backlog_handle(tx: &Transaction<'_>) -> Result<Option<String>, VivariumError> {
    tx.query_row(
        "SELECT handle FROM work_graphs WHERE code = ?1",
        params![BACKLOG_GRAPH_CODE],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| VivariumError::Other(format!("failed to load backlog graph: {e}")))
}

fn existing_node_ids(
    tx: &Transaction<'_>,
    graph_handle: &str,
) -> Result<HashSet<String>, VivariumError> {
    let mut stmt = tx
        .prepare("SELECT source_id FROM work_graph_nodes WHERE graph_handle = ?1")
        .map_err(|e| VivariumError::Other(format!("failed to prepare backlog nodes: {e}")))?;
    let rows = stmt
        .query_map(params![graph_handle], |row| row.get::<_, String>(0))
        .map_err(|e| VivariumError::Other(format!("failed to query backlog nodes: {e}")))?;
    let mut out = HashSet::new();
    for row in rows {
        out.insert(
            row.map_err(|e| VivariumError::Other(format!("failed to read backlog node: {e}")))?,
        );
    }
    Ok(out)
}

fn existing_edge_pairs(
    tx: &Transaction<'_>,
    graph_handle: &str,
) -> Result<HashSet<(String, String)>, VivariumError> {
    let mut stmt = tx
        .prepare("SELECT from_node, to_node FROM work_graph_edges WHERE graph_handle = ?1")
        .map_err(|e| VivariumError::Other(format!("failed to prepare backlog edges: {e}")))?;
    let rows = stmt
        .query_map(params![graph_handle], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| VivariumError::Other(format!("failed to query backlog edges: {e}")))?;
    let mut out = HashSet::new();
    for row in rows {
        let pair =
            row.map_err(|e| VivariumError::Other(format!("failed to read backlog edge: {e}")))?;
        out.insert(pair);
    }
    Ok(out)
}

fn insert_backlog_node(
    tx: &Transaction<'_>,
    graph_handle: &str,
    node: &BacklogNodeInput,
    now: &str,
) -> Result<(), VivariumError> {
    let handle = node_handle_for(graph_handle, &node.source_id);
    tx.execute(
        "INSERT INTO work_graph_nodes
           (handle, graph_handle, source_id, label, state, subgraph, kind, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?7)",
        params![
            handle,
            graph_handle,
            node.source_id,
            node.label,
            node.state,
            node.kind,
            now
        ],
    )
    .map_err(|e| VivariumError::Other(format!("failed to insert backlog node: {e}")))?;
    tx.execute(
        "INSERT INTO work_graph_events (graph_handle, occurred_at, event_type, node_handle, note)
         VALUES (?1, ?2, 'backlog_attached', ?3, ?4)",
        params![graph_handle, now, handle, format!("kind={}", node.kind)],
    )
    .map_err(|e| VivariumError::Other(format!("failed to insert backlog event: {e}")))?;
    Ok(())
}

fn synthesize_backlog_mermaid(nodes: &[BacklogNodeInput], edges: &[WorkGraphEdgeInput]) -> String {
    let mut out = String::from("flowchart TD\n");
    for node in nodes {
        let label = node.label.replace('"', "'");
        out.push_str(&format!("  {}[\"{label}\"]\n", node.source_id));
    }
    for edge in edges {
        out.push_str(&format!(
            "  {} --> {}\n",
            edge.from_source_id, edge.to_source_id
        ));
    }
    out
}
