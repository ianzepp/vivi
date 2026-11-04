//! Work-graph tables: durable topology, revisions, and graph events.

use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, params};
use serde::Serialize;

use super::{Storage, VivariumError, sha256_hex};

/// Row stored for one work graph.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkGraphRow {
    pub handle: String,
    pub code: String,
    pub status: String,
    pub current_revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// One node in a work graph.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkGraphNodeRow {
    pub handle: String,
    pub graph_handle: String,
    pub source_id: String,
    pub label: String,
    pub state: String,
    pub subgraph: Option<String>,
    pub kind: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Directed dependency edge: `to_node` requires `from_node` when the style
/// is `solid`; `dotted` edges are non-gating couplings.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkGraphEdgeRow {
    pub handle: String,
    pub graph_handle: String,
    pub from_node: String,
    pub to_node: String,
    pub label: Option<String>,
    pub style: String,
    pub created_at: String,
}

/// Inputs for an atomic first import of a work graph.
#[derive(Debug, Clone)]
pub struct WorkGraphImportInput {
    pub code: String,
    pub mermaid_source: String,
    pub content_hash: String,
    pub nodes: Vec<WorkGraphNodeInput>,
    pub edges: Vec<WorkGraphEdgeInput>,
}

/// Node fields supplied by the Mermaid compiler before handles are assigned.
#[derive(Debug, Clone)]
pub struct WorkGraphNodeInput {
    pub source_id: String,
    pub label: String,
    pub subgraph: Option<String>,
    pub kind: String,
}

/// Edge fields using Mermaid source IDs (resolved to handles on write).
#[derive(Debug, Clone)]
pub struct WorkGraphEdgeInput {
    pub from_source_id: String,
    pub to_source_id: String,
    pub label: Option<String>,
    pub style: String,
}

/// Result of committing a graph import.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkGraphImportCommit {
    pub graph: WorkGraphRow,
    pub nodes: Vec<WorkGraphNodeRow>,
    pub edges: Vec<WorkGraphEdgeRow>,
    pub created: bool,
}

impl Storage {
    /// Look up a work graph by project-unique code.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the query fails.
    pub fn work_graph_by_code(&self, code: &str) -> Result<Option<WorkGraphRow>, VivariumError> {
        self.conn
            .borrow()
            .query_row(
                "SELECT handle, code, status, current_revision, created_at, updated_at
                 FROM work_graphs WHERE code = ?1",
                params![code],
                map_graph_row,
            )
            .optional()
            .map_err(|e| VivariumError::Other(format!("failed to load work graph by code: {e}")))
    }

    /// Look up a work graph by immutable handle.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the query fails.
    pub fn work_graph_by_handle(
        &self,
        handle: &str,
    ) -> Result<Option<WorkGraphRow>, VivariumError> {
        self.conn
            .borrow()
            .query_row(
                "SELECT handle, code, status, current_revision, created_at, updated_at
                 FROM work_graphs WHERE handle = ?1",
                params![handle],
                map_graph_row,
            )
            .optional()
            .map_err(|e| VivariumError::Other(format!("failed to load work graph by handle: {e}")))
    }

    /// Content hash of the graph's current revision, if any.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the query fails.
    pub fn work_graph_revision_hash(
        &self,
        graph_handle: &str,
        revision: i64,
    ) -> Result<Option<String>, VivariumError> {
        self.conn
            .borrow()
            .query_row(
                "SELECT content_hash FROM work_graph_revisions
                 WHERE graph_handle = ?1 AND revision = ?2",
                params![graph_handle, revision],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| VivariumError::Other(format!("failed to load graph revision hash: {e}")))
    }

    /// Load all nodes for a graph.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the query fails.
    pub fn work_graph_nodes(
        &self,
        graph_handle: &str,
    ) -> Result<Vec<WorkGraphNodeRow>, VivariumError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare(
                "SELECT handle, graph_handle, source_id, label, state, subgraph, kind,
                        created_at, updated_at
                 FROM work_graph_nodes WHERE graph_handle = ?1
                 ORDER BY source_id",
            )
            .map_err(|e| VivariumError::Other(format!("failed to prepare graph nodes: {e}")))?;
        let rows = stmt
            .query_map(params![graph_handle], map_node_row)
            .map_err(|e| VivariumError::Other(format!("failed to query graph nodes: {e}")))?;
        collect_rows(rows, "graph nodes")
    }

    /// Load all edges for a graph.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the query fails.
    pub fn work_graph_edges(
        &self,
        graph_handle: &str,
    ) -> Result<Vec<WorkGraphEdgeRow>, VivariumError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare(
                "SELECT handle, graph_handle, from_node, to_node, label, style, created_at
                 FROM work_graph_edges WHERE graph_handle = ?1
                 ORDER BY from_node, to_node",
            )
            .map_err(|e| VivariumError::Other(format!("failed to prepare graph edges: {e}")))?;
        let rows = stmt
            .query_map(params![graph_handle], map_edge_row)
            .map_err(|e| VivariumError::Other(format!("failed to query graph edges: {e}")))?;
        collect_rows(rows, "graph edges")
    }

    /// Atomically insert a new work graph at revision 1.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the transaction fails.
    pub fn import_work_graph(
        &self,
        input: &WorkGraphImportInput,
    ) -> Result<WorkGraphImportCommit, VivariumError> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn
            .transaction()
            .map_err(|e| VivariumError::Other(format!("failed to begin graph import: {e}")))?;
        let commit = insert_work_graph(&tx, input)?;
        tx.commit()
            .map_err(|e| VivariumError::Other(format!("failed to commit graph import: {e}")))?;
        Ok(commit)
    }

    /// Apply a validated mutation plan in one transaction.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the transaction fails.
    pub fn apply_work_graph_plan(
        &self,
        plan: &WorkGraphApplyPlan,
    ) -> Result<WorkGraphRow, VivariumError> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn
            .transaction()
            .map_err(|e| VivariumError::Other(format!("failed to begin graph apply: {e}")))?;
        let graph = execute_apply_plan(&tx, plan)?;
        tx.commit()
            .map_err(|e| VivariumError::Other(format!("failed to commit graph apply: {e}")))?;
        Ok(graph)
    }

    /// Mark a node state (e.g. `done`) and append a graph event.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the update fails.
    pub fn set_work_graph_node_state(
        &self,
        graph_handle: &str,
        node_handle: &str,
        state: &str,
        note: Option<&str>,
    ) -> Result<(), VivariumError> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.borrow_mut();
        let tx = conn
            .transaction()
            .map_err(|e| VivariumError::Other(format!("failed to begin node state tx: {e}")))?;
        set_node_state_tx(&tx, graph_handle, node_handle, state, note, &now)?;
        tx.commit()
            .map_err(|e| VivariumError::Other(format!("failed to commit node state: {e}")))?;
        Ok(())
    }

    /// Correct a node's kind without touching state or readiness. Audit
    /// repair uses this for nodes minted before kinds were persisted.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the update fails or the node is
    /// missing.
    pub fn set_work_graph_node_kind(
        &self,
        graph_handle: &str,
        node_handle: &str,
        kind: &str,
    ) -> Result<(), VivariumError> {
        let now = Utc::now().to_rfc3339();
        let changed = self
            .conn
            .borrow_mut()
            .execute(
                "UPDATE work_graph_nodes SET kind = ?3, updated_at = ?4
                 WHERE graph_handle = ?1 AND handle = ?2",
                params![graph_handle, node_handle, kind, now],
            )
            .map_err(|e| VivariumError::Other(format!("failed to set node kind: {e}")))?;
        if changed == 0 {
            return Err(VivariumError::Message(format!(
                "graph node not found: {node_handle}"
            )));
        }
        Ok(())
    }

    /// List all work graphs ordered by code.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the query fails.
    pub fn work_graphs(&self) -> Result<Vec<WorkGraphRow>, VivariumError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare(
                "SELECT handle, code, status, current_revision, created_at, updated_at
                 FROM work_graphs ORDER BY code",
            )
            .map_err(|e| VivariumError::Other(format!("failed to prepare work graphs: {e}")))?;
        let rows = stmt
            .query_map([], map_graph_row)
            .map_err(|e| VivariumError::Other(format!("failed to query work graphs: {e}")))?;
        collect_rows(rows, "work graphs")
    }

    /// List attempts for a node.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the query fails.
    pub fn work_graph_attempts(
        &self,
        node_handle: &str,
    ) -> Result<Vec<WorkGraphAttemptRow>, VivariumError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare(
                "SELECT attempt_id, graph_handle, node_handle, task_message_id, task_handle,
                        role, state, note, created_at, updated_at
                 FROM work_graph_attempts WHERE node_handle = ?1
                 ORDER BY attempt_id",
            )
            .map_err(|e| VivariumError::Other(format!("failed to prepare attempts: {e}")))?;
        let rows = stmt
            .query_map(params![node_handle], map_attempt_row)
            .map_err(|e| VivariumError::Other(format!("failed to query attempts: {e}")))?;
        collect_rows(rows, "graph attempts")
    }

    /// Graph lifecycle events after a cursor (exclusive).
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the query fails.
    pub fn list_work_graph_events_after(
        &self,
        after_event_id: i64,
    ) -> Result<Vec<WorkGraphEventRow>, VivariumError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare(
                "SELECT event_id, graph_handle, occurred_at, event_type, node_handle, note
                 FROM work_graph_events
                 WHERE event_id > ?1
                 ORDER BY event_id",
            )
            .map_err(|e| VivariumError::Other(format!("failed to prepare graph events: {e}")))?;
        let rows = stmt
            .query_map(params![after_event_id], map_graph_event_row)
            .map_err(|e| VivariumError::Other(format!("failed to query graph events: {e}")))?;
        collect_rows(rows, "graph events")
    }

    /// Activate a node and bind a task attempt in one transaction.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the transaction fails.
    pub fn activate_work_graph_node(
        &self,
        input: &WorkGraphActivateInput,
    ) -> Result<(), VivariumError> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.borrow_mut();
        let tx = conn
            .transaction()
            .map_err(|e| VivariumError::Other(format!("failed to begin activate tx: {e}")))?;
        set_node_state_tx(
            &tx,
            &input.graph_handle,
            &input.node_handle,
            "active",
            input.note.as_deref(),
            &now,
        )?;
        tx.execute(
            "INSERT INTO work_graph_attempts
               (graph_handle, node_handle, task_message_id, task_handle, role, state,
                note, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?7, ?7)",
            params![
                input.graph_handle,
                input.node_handle,
                input.task_message_id,
                input.task_handle,
                input.role,
                input.note,
                now
            ],
        )
        .map_err(|e| VivariumError::Other(format!("failed to insert graph attempt: {e}")))?;
        tx.execute(
            "INSERT INTO work_graph_events
               (graph_handle, occurred_at, event_type, node_handle, note)
             VALUES (?1, ?2, 'attempt_bound', ?3, ?4)",
            params![
                input.graph_handle,
                now,
                input.node_handle,
                format!(
                    "task={} content={}",
                    input.task_handle, input.task_content_hash
                )
            ],
        )
        .map_err(|e| VivariumError::Other(format!("failed to insert attempt event: {e}")))?;
        tx.commit()
            .map_err(|e| VivariumError::Other(format!("failed to commit activate: {e}")))?;
        Ok(())
    }

    /// Complete a node and emit `node_ready` events for newly ready
    /// successors. When `decision` is provided, a `step_decision` event is
    /// written in the same transaction — the durable record of who decided
    /// the transition and why.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the transaction fails.
    pub fn complete_work_graph_node(
        &self,
        graph_handle: &str,
        node_handle: &str,
        note: Option<&str>,
        newly_ready_node_handles: &[String],
        decision: Option<&str>,
    ) -> Result<(), VivariumError> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.borrow_mut();
        let tx = conn
            .transaction()
            .map_err(|e| VivariumError::Other(format!("failed to begin complete tx: {e}")))?;
        set_node_state_tx(&tx, graph_handle, node_handle, "done", note, &now)?;
        for ready_handle in newly_ready_node_handles {
            tx.execute(
                "INSERT INTO work_graph_events
                   (graph_handle, occurred_at, event_type, node_handle, note)
                 VALUES (?1, ?2, 'node_ready', ?3, ?4)",
                params![
                    graph_handle,
                    now,
                    ready_handle,
                    format!("unlocked_by={node_handle}")
                ],
            )
            .map_err(|e| VivariumError::Other(format!("failed to insert ready event: {e}")))?;
        }
        if let Some(decision) = decision {
            tx.execute(
                "INSERT INTO work_graph_events
                   (graph_handle, occurred_at, event_type, node_handle, note)
                 VALUES (?1, ?2, 'step_decision', ?3, ?4)",
                params![graph_handle, now, node_handle, decision],
            )
            .map_err(|e| VivariumError::Other(format!("failed to insert decision event: {e}")))?;
        }
        tx.commit()
            .map_err(|e| VivariumError::Other(format!("failed to commit complete: {e}")))?;
        Ok(())
    }
}

/// Task-attempt binding row.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkGraphAttemptRow {
    pub attempt_id: i64,
    pub graph_handle: String,
    pub node_handle: String,
    pub task_message_id: String,
    pub task_handle: String,
    pub role: Option<String>,
    pub state: String,
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Graph lifecycle event row.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkGraphEventRow {
    pub event_id: i64,
    pub graph_handle: String,
    pub occurred_at: String,
    pub event_type: String,
    pub node_handle: Option<String>,
    pub note: Option<String>,
}

/// Inputs for activating a node with a bound task.
#[derive(Debug, Clone)]
pub struct WorkGraphActivateInput {
    pub graph_handle: String,
    pub node_handle: String,
    pub task_message_id: String,
    pub task_handle: String,
    /// Content hash of the task record — the durable "task body as
    /// dispatched" pin recorded with the binding.
    pub task_content_hash: String,
    pub role: Option<String>,
    pub note: Option<String>,
}

/// Validated apply mutation ready for a single transaction.
#[derive(Debug, Clone)]
pub struct WorkGraphApplyPlan {
    pub graph_handle: String,
    pub code: String,
    pub next_revision: i64,
    pub mermaid_source: String,
    pub content_hash: String,
    pub nodes_add: Vec<WorkGraphNodeInput>,
    pub nodes_update: Vec<WorkGraphNodeInput>,
    pub nodes_remove: Vec<String>,
    pub edges_add: Vec<WorkGraphEdgeInput>,
    pub edges_remove: Vec<(String, String)>,
    pub event_note: String,
}

/// Deterministic graph handle from project-unique code.
#[must_use]
pub fn graph_handle_for_code(code: &str) -> String {
    let digest = sha256_hex(format!("graph\0{code}").as_bytes());
    format!("gph_{}", &digest[..16])
}

/// Deterministic node handle from graph handle + Mermaid source id.
#[must_use]
pub fn node_handle_for(graph_handle: &str, source_id: &str) -> String {
    let digest = sha256_hex(format!("node\0{graph_handle}\0{source_id}").as_bytes());
    format!("nod_{}", &digest[..16])
}

/// Deterministic edge handle from graph + endpoint node handles.
#[must_use]
pub fn edge_handle_for(graph_handle: &str, from_node: &str, to_node: &str) -> String {
    let digest = sha256_hex(format!("edge\0{graph_handle}\0{from_node}\0{to_node}").as_bytes());
    format!("edg_{}", &digest[..16])
}

pub(super) fn insert_work_graph(
    tx: &Transaction<'_>,
    input: &WorkGraphImportInput,
) -> Result<WorkGraphImportCommit, VivariumError> {
    let now = Utc::now().to_rfc3339();
    let graph_handle = graph_handle_for_code(&input.code);
    let graph = WorkGraphRow {
        handle: graph_handle.clone(),
        code: input.code.clone(),
        status: "open".into(),
        current_revision: 1,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    tx.execute(
        "INSERT INTO work_graphs (handle, code, status, current_revision, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            graph.handle,
            graph.code,
            graph.status,
            graph.current_revision,
            graph.created_at,
            graph.updated_at
        ],
    )
    .map_err(|e| VivariumError::Other(format!("failed to insert work graph: {e}")))?;
    tx.execute(
        "INSERT INTO work_graph_revisions
           (graph_handle, revision, mermaid_source, content_hash, created_at)
         VALUES (?1, 1, ?2, ?3, ?4)",
        params![graph.handle, input.mermaid_source, input.content_hash, now],
    )
    .map_err(|e| VivariumError::Other(format!("failed to insert graph revision: {e}")))?;
    let nodes = insert_nodes(tx, &graph.handle, &input.nodes, &now)?;
    let edges = insert_edges(tx, &graph.handle, &input.edges, &now)?;
    tx.execute(
        "INSERT INTO work_graph_events (graph_handle, occurred_at, event_type, node_handle, note)
         VALUES (?1, ?2, 'revision_imported', NULL, ?3)",
        params![
            graph.handle,
            now,
            format!("revision=1 nodes={} edges={}", nodes.len(), edges.len())
        ],
    )
    .map_err(|e| VivariumError::Other(format!("failed to insert graph event: {e}")))?;
    Ok(WorkGraphImportCommit {
        graph,
        nodes,
        edges,
        created: true,
    })
}

fn insert_nodes(
    tx: &Transaction<'_>,
    graph_handle: &str,
    nodes: &[WorkGraphNodeInput],
    now: &str,
) -> Result<Vec<WorkGraphNodeRow>, VivariumError> {
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        let row = WorkGraphNodeRow {
            handle: node_handle_for(graph_handle, &node.source_id),
            graph_handle: graph_handle.to_string(),
            source_id: node.source_id.clone(),
            label: node.label.clone(),
            state: "open".into(),
            subgraph: node.subgraph.clone(),
            kind: node.kind.clone(),
            created_at: now.to_string(),
            updated_at: now.to_string(),
        };
        tx.execute(
            "INSERT INTO work_graph_nodes
               (handle, graph_handle, source_id, label, state, subgraph, kind,
                created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                row.handle,
                row.graph_handle,
                row.source_id,
                row.label,
                row.state,
                row.subgraph,
                row.kind,
                row.created_at,
                row.updated_at
            ],
        )
        .map_err(|e| VivariumError::Other(format!("failed to insert graph node: {e}")))?;
        out.push(row);
    }
    Ok(out)
}

pub(super) fn insert_edges(
    tx: &Transaction<'_>,
    graph_handle: &str,
    edges: &[WorkGraphEdgeInput],
    now: &str,
) -> Result<Vec<WorkGraphEdgeRow>, VivariumError> {
    let mut out = Vec::with_capacity(edges.len());
    for edge in edges {
        let from_node = node_handle_for(graph_handle, &edge.from_source_id);
        let to_node = node_handle_for(graph_handle, &edge.to_source_id);
        let row = WorkGraphEdgeRow {
            handle: edge_handle_for(graph_handle, &from_node, &to_node),
            graph_handle: graph_handle.to_string(),
            from_node,
            to_node,
            label: edge.label.clone(),
            style: edge.style.clone(),
            created_at: now.to_string(),
        };
        tx.execute(
            "INSERT INTO work_graph_edges
               (handle, graph_handle, from_node, to_node, label, style, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                row.handle,
                row.graph_handle,
                row.from_node,
                row.to_node,
                row.label,
                row.style,
                row.created_at
            ],
        )
        .map_err(|e| VivariumError::Other(format!("failed to insert graph edge: {e}")))?;
        out.push(row);
    }
    Ok(out)
}

fn map_graph_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkGraphRow> {
    Ok(WorkGraphRow {
        handle: row.get(0)?,
        code: row.get(1)?,
        status: row.get(2)?,
        current_revision: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

pub(super) fn map_node_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkGraphNodeRow> {
    Ok(WorkGraphNodeRow {
        handle: row.get(0)?,
        graph_handle: row.get(1)?,
        source_id: row.get(2)?,
        label: row.get(3)?,
        state: row.get(4)?,
        subgraph: row.get(5)?,
        kind: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn map_edge_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkGraphEdgeRow> {
    Ok(WorkGraphEdgeRow {
        handle: row.get(0)?,
        graph_handle: row.get(1)?,
        from_node: row.get(2)?,
        to_node: row.get(3)?,
        label: row.get(4)?,
        style: row.get(5)?,
        created_at: row.get(6)?,
    })
}

fn collect_rows<T, I>(rows: I, label: &str) -> Result<Vec<T>, VivariumError>
where
    I: Iterator<Item = Result<T, rusqlite::Error>>,
{
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| VivariumError::Other(format!("failed to read {label}: {e}")))?);
    }
    Ok(out)
}

fn set_node_state_tx(
    tx: &Transaction<'_>,
    graph_handle: &str,
    node_handle: &str,
    state: &str,
    note: Option<&str>,
    now: &str,
) -> Result<(), VivariumError> {
    let changed = tx
        .execute(
            "UPDATE work_graph_nodes SET state = ?1, updated_at = ?2
             WHERE handle = ?3 AND graph_handle = ?4",
            params![state, now, node_handle, graph_handle],
        )
        .map_err(|e| VivariumError::Other(format!("failed to update node state: {e}")))?;
    if changed == 0 {
        return Err(VivariumError::Message(format!(
            "graph node not found: {node_handle}"
        )));
    }
    tx.execute(
        "UPDATE work_graphs SET updated_at = ?1 WHERE handle = ?2",
        params![now, graph_handle],
    )
    .map_err(|e| VivariumError::Other(format!("failed to touch graph: {e}")))?;
    tx.execute(
        "INSERT INTO work_graph_events
           (graph_handle, occurred_at, event_type, node_handle, note)
         VALUES (?1, ?2, 'node_state', ?3, ?4)",
        params![graph_handle, now, node_handle, note],
    )
    .map_err(|e| VivariumError::Other(format!("failed to insert node state event: {e}")))?;
    Ok(())
}

fn map_attempt_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkGraphAttemptRow> {
    Ok(WorkGraphAttemptRow {
        attempt_id: row.get(0)?,
        graph_handle: row.get(1)?,
        node_handle: row.get(2)?,
        task_message_id: row.get(3)?,
        task_handle: row.get(4)?,
        role: row.get(5)?,
        state: row.get(6)?,
        note: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn map_graph_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkGraphEventRow> {
    Ok(WorkGraphEventRow {
        event_id: row.get(0)?,
        graph_handle: row.get(1)?,
        occurred_at: row.get(2)?,
        event_type: row.get(3)?,
        node_handle: row.get(4)?,
        note: row.get(5)?,
    })
}

fn execute_apply_plan(
    tx: &Transaction<'_>,
    plan: &WorkGraphApplyPlan,
) -> Result<WorkGraphRow, VivariumError> {
    let now = Utc::now().to_rfc3339();
    apply_removals(tx, plan)?;
    insert_nodes(tx, &plan.graph_handle, &plan.nodes_add, &now)?;
    apply_node_updates(tx, plan, &now)?;
    insert_edges(tx, &plan.graph_handle, &plan.edges_add, &now)?;
    record_apply_revision(tx, plan, &now)?;
    tx.query_row(
        "SELECT handle, code, status, current_revision, created_at, updated_at
         FROM work_graphs WHERE handle = ?1",
        params![plan.graph_handle],
        map_graph_row,
    )
    .map_err(|e| VivariumError::Other(format!("failed to reload graph after apply: {e}")))
}

fn apply_removals(tx: &Transaction<'_>, plan: &WorkGraphApplyPlan) -> Result<(), VivariumError> {
    for source_id in &plan.nodes_remove {
        let handle = node_handle_for(&plan.graph_handle, source_id);
        tx.execute(
            "DELETE FROM work_graph_nodes WHERE handle = ?1",
            params![handle],
        )
        .map_err(|e| VivariumError::Other(format!("failed to remove graph node: {e}")))?;
    }
    for (from_id, to_id) in &plan.edges_remove {
        let from_node = node_handle_for(&plan.graph_handle, from_id);
        let to_node = node_handle_for(&plan.graph_handle, to_id);
        tx.execute(
            "DELETE FROM work_graph_edges
             WHERE graph_handle = ?1 AND from_node = ?2 AND to_node = ?3",
            params![plan.graph_handle, from_node, to_node],
        )
        .map_err(|e| VivariumError::Other(format!("failed to remove graph edge: {e}")))?;
    }
    Ok(())
}

fn apply_node_updates(
    tx: &Transaction<'_>,
    plan: &WorkGraphApplyPlan,
    now: &str,
) -> Result<(), VivariumError> {
    for node in &plan.nodes_update {
        let handle = node_handle_for(&plan.graph_handle, &node.source_id);
        tx.execute(
            "UPDATE work_graph_nodes
             SET label = ?1, subgraph = ?2, kind = ?3, updated_at = ?4
             WHERE handle = ?5",
            params![node.label, node.subgraph, node.kind, now, handle],
        )
        .map_err(|e| VivariumError::Other(format!("failed to update graph node: {e}")))?;
    }
    Ok(())
}

fn record_apply_revision(
    tx: &Transaction<'_>,
    plan: &WorkGraphApplyPlan,
    now: &str,
) -> Result<(), VivariumError> {
    tx.execute(
        "INSERT INTO work_graph_revisions
           (graph_handle, revision, mermaid_source, content_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            plan.graph_handle,
            plan.next_revision,
            plan.mermaid_source,
            plan.content_hash,
            now
        ],
    )
    .map_err(|e| VivariumError::Other(format!("failed to insert apply revision: {e}")))?;
    tx.execute(
        "UPDATE work_graphs
         SET current_revision = ?1, updated_at = ?2
         WHERE handle = ?3",
        params![plan.next_revision, now, plan.graph_handle],
    )
    .map_err(|e| VivariumError::Other(format!("failed to bump graph revision: {e}")))?;
    tx.execute(
        "INSERT INTO work_graph_events
           (graph_handle, occurred_at, event_type, node_handle, note)
         VALUES (?1, ?2, 'revision_applied', NULL, ?3)",
        params![plan.graph_handle, now, plan.event_note],
    )
    .map_err(|e| VivariumError::Other(format!("failed to insert apply event: {e}")))?;
    Ok(())
}
