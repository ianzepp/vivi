//! Work-graph domain: import, show, ready/blocked projections.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

use super::Mailspace;
use super::holding::with_readiness;
use super::mermaid::{MermaidFlowchart, parse_flowchart};
use crate::error::VivariumError;
use crate::storage::{
    BACKLOG_GRAPH_CODE, Storage, WorkGraphEdgeInput, WorkGraphEdgeRow, WorkGraphImportCommit,
    WorkGraphImportInput, WorkGraphNodeInput, WorkGraphNodeRow, WorkGraphRow, sha256_hex,
};

/// Import report returned by check and commit paths.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphImportReport {
    pub check_only: bool,
    pub created: bool,
    pub idempotent: bool,
    pub graph_handle: String,
    pub code: String,
    pub revision: i64,
    pub content_hash: String,
    pub node_count: usize,
    pub edge_count: usize,
    pub roots: Vec<String>,
    pub ready: Vec<GraphNodeView>,
    pub nodes: Vec<GraphNodeView>,
    pub edges: Vec<GraphEdgeView>,
}

/// Show / projection view of a stored graph.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphShow {
    pub graph: WorkGraphRow,
    pub content_hash: Option<String>,
    pub nodes: Vec<GraphNodeView>,
    pub edges: Vec<GraphEdgeView>,
    pub ready: Vec<GraphNodeView>,
    pub blocked: Vec<GraphNodeView>,
}

/// Node with derived readiness fields.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphNodeView {
    pub handle: String,
    pub source_id: String,
    pub label: String,
    pub state: String,
    pub subgraph: Option<String>,
    pub kind: String,
    pub readiness: String,
    pub blocked_by: Vec<String>,
    pub successors: Vec<String>,
}

/// Edge view using source IDs for agent readability.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphEdgeView {
    pub handle: String,
    pub from_handle: String,
    pub to_handle: String,
    pub from_source_id: String,
    pub to_source_id: String,
    pub label: Option<String>,
    pub style: String,
}

impl Mailspace {
    /// Parse, validate, and optionally commit a Mermaid work graph import.
    ///
    /// # Errors
    /// Returns parse, validation, conflict, or storage errors.
    pub fn graph_import(
        &self,
        code: &str,
        mermaid_source: &str,
        check_only: bool,
    ) -> Result<GraphImportReport, VivariumError> {
        validate_code(code)?;
        let flowchart = parse_flowchart(mermaid_source)?;
        let content_hash = sha256_hex(mermaid_source.as_bytes());
        let storage = self.storage()?;
        if let Some(existing) = storage.work_graph_by_code(code)? {
            return idempotent_or_conflict(
                &storage,
                &existing,
                &content_hash,
                &flowchart,
                check_only,
            );
        }
        let input = compile_import(code, mermaid_source, &content_hash, &flowchart);
        if check_only {
            return Ok(preview_import(&input, &flowchart, true));
        }
        let commit = storage.import_work_graph(&input)?;
        Ok(report_from_commit(&commit, &content_hash, false, false))
    }

    /// Import Mermaid from a file path.
    ///
    /// # Errors
    /// Returns IO, parse, or storage errors.
    pub fn graph_import_file(
        &self,
        code: &str,
        path: &Path,
        check_only: bool,
    ) -> Result<GraphImportReport, VivariumError> {
        let source = std::fs::read_to_string(path).map_err(|e| {
            VivariumError::Other(format!("failed to read graph file {}: {e}", path.display()))
        })?;
        self.graph_import(code, &source, check_only)
    }

    /// Load a graph by code or handle and project readiness.
    ///
    /// # Errors
    /// Returns not-found or storage errors.
    pub fn graph_show(&self, code_or_handle: &str) -> Result<GraphShow, VivariumError> {
        let storage = self.storage()?;
        let graph = resolve_graph(&storage, code_or_handle)?;
        let nodes = storage.work_graph_nodes(&graph.handle)?;
        let edges = storage.work_graph_edges(&graph.handle)?;
        let hash = storage.work_graph_revision_hash(&graph.handle, graph.current_revision)?;
        let (mut node_views, mut ready, mut blocked) = project_nodes(&nodes, &edges);
        if graph.code == BACKLOG_GRAPH_CODE {
            // Foreign and sent-copy nodes are not this mailspace's dispatch.
            self.mark_holdings(&storage, &mut node_views)?;
            ready = with_readiness(&node_views, "ready");
            blocked = with_readiness(&node_views, "blocked");
        }
        let edge_views = project_edges(&nodes, &edges);
        Ok(GraphShow {
            graph,
            content_hash: hash,
            nodes: node_views,
            edges: edge_views,
            ready,
            blocked,
        })
    }

    /// Blocked nodes of a graph, each with the prerequisites still
    /// unfinished, optionally keeping one kind. Sorted by source id so the
    /// listing is stable across reads.
    ///
    /// # Errors
    /// Returns not-found or storage errors.
    pub fn graph_blocked(
        &self,
        code_or_handle: &str,
        kind: Option<&str>,
    ) -> Result<Vec<GraphBlockedNode>, VivariumError> {
        let show = self.graph_show(code_or_handle)?;
        let mut blocked: Vec<GraphBlockedNode> = show
            .blocked
            .into_iter()
            .filter(|node| kind.is_none_or(|kind| node.kind == kind))
            .map(|node| GraphBlockedNode {
                source_id: node.source_id,
                kind: node.kind,
                label: node.label,
                blocked_by: node.blocked_by,
            })
            .collect();
        blocked.sort_by(|left, right| left.source_id.cmp(&right.source_id));
        Ok(blocked)
    }
}

/// Compact frontier projection for status loops (never full topology).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphFrontier {
    pub code: String,
    pub handle: String,
    pub revision: i64,
    pub ready: Vec<String>,
    pub blocked: Vec<String>,
    pub active: Vec<String>,
    /// Open work this mailspace handed to another one: tracked here, never
    /// dispatched here.
    pub awaiting: Vec<String>,
    /// Work a role reported done that its author or a mind has not accepted:
    /// open or active, with its dependents still blocked.
    pub reported: Vec<String>,
    pub gates: Vec<GraphFrontierGate>,
}

/// One operator-gated ready node (decision/stub/parked): ready by
/// prerequisites, but never dispatchable as work.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphFrontierGate {
    pub source_id: String,
    pub kind: String,
}

/// One blocked node and the prerequisites it is still waiting on.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphBlockedNode {
    pub source_id: String,
    pub kind: String,
    pub label: String,
    /// Source ids of prerequisites whose state is not `done`.
    pub blocked_by: Vec<String>,
}

/// Compact receipt after complete / activate / node|edge append.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphActionReceipt {
    pub action: String,
    pub code: String,
    pub handle: String,
    pub revision: i64,
    pub node: Option<String>,
    pub task: Option<String>,
    /// Content hash of the bound task on activate receipts: the pin for
    /// "task body as dispatched". Absent on other actions.
    pub content: Option<String>,
    pub ready: Vec<String>,
    pub blocked: Vec<String>,
    pub active: Vec<String>,
    pub gates: Vec<GraphFrontierGate>,
}

/// Compact import receipt for CLI (no full node/edge topology).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphImportReceipt {
    pub check_only: bool,
    pub created: bool,
    pub idempotent: bool,
    pub graph_handle: String,
    pub code: String,
    pub revision: i64,
    pub content_hash: String,
    pub node_count: usize,
    pub edge_count: usize,
    pub roots: Vec<String>,
    pub ready: Vec<String>,
    pub gates: Vec<GraphFrontierGate>,
}

/// Build a frontier projection from a full show view. Ready nodes split into
/// dispatchable work and operator gates (decision/stub/parked kinds).
#[must_use]
pub fn frontier_from_show(show: &GraphShow) -> GraphFrontier {
    let (work, gates) = split_ready_gates(&show.ready);
    GraphFrontier {
        code: show.graph.code.clone(),
        handle: show.graph.handle.clone(),
        revision: show.graph.current_revision,
        ready: work,
        gates,
        blocked: source_ids(&show.blocked),
        active: show
            .nodes
            .iter()
            .filter(|n| n.state == "active")
            .map(|n| n.source_id.clone())
            .collect(),
        awaiting: show
            .nodes
            .iter()
            .filter(|n| n.readiness == super::holding::READINESS_AWAITING)
            .map(|n| n.source_id.clone())
            .collect(),
        reported: show
            .nodes
            .iter()
            .filter(|n| n.readiness == super::holding::READINESS_REPORTED)
            .map(|n| n.source_id.clone())
            .collect(),
    }
}

/// Partition ready node views into dispatchable source ids and gates.
#[must_use]
pub fn split_ready_gates(ready: &[GraphNodeView]) -> (Vec<String>, Vec<GraphFrontierGate>) {
    let mut work = Vec::new();
    let mut gates = Vec::new();
    for node in ready {
        if super::mermaid::is_gate_kind(&node.kind) {
            gates.push(GraphFrontierGate {
                source_id: node.source_id.clone(),
                kind: node.kind.clone(),
            });
        } else {
            work.push(node.source_id.clone());
        }
    }
    (work, gates)
}

/// Build a lifecycle/mutation receipt from a show view.
#[must_use]
pub fn action_receipt_from_show(
    action: &str,
    show: &GraphShow,
    node: Option<&str>,
    task: Option<&str>,
) -> GraphActionReceipt {
    let frontier = frontier_from_show(show);
    GraphActionReceipt {
        action: action.into(),
        code: frontier.code,
        handle: frontier.handle,
        revision: frontier.revision,
        node: node.map(str::to_string),
        task: task.map(str::to_string),
        content: None,
        ready: frontier.ready,
        blocked: frontier.blocked,
        active: frontier.active,
        gates: frontier.gates,
    }
}

/// Print import report as compact text or JSON receipt.
///
/// # Errors
/// Returns JSON encode errors or large-stdout refusal.
pub fn print_import_report(
    report: &GraphImportReport,
    json: bool,
    confirm_large: bool,
) -> Result<(), VivariumError> {
    let (ready, gates) = split_ready_gates(&report.ready);
    let receipt = GraphImportReceipt {
        check_only: report.check_only,
        created: report.created,
        idempotent: report.idempotent,
        graph_handle: report.graph_handle.clone(),
        code: report.code.clone(),
        revision: report.revision,
        content_hash: report.content_hash.clone(),
        node_count: report.node_count,
        edge_count: report.edge_count,
        roots: report.roots.clone(),
        ready,
        gates,
    };
    if json {
        return crate::stdout_budget::print_pretty_json(
            "graph import",
            &receipt,
            confirm_large,
            Some(&report.code),
        );
    }
    let mode = if receipt.check_only {
        "check"
    } else if receipt.idempotent {
        "idempotent"
    } else {
        "imported"
    };
    println!("graph {mode}");
    println!("  handle   {}", receipt.graph_handle);
    println!("  code     {}", receipt.code);
    println!("  revision {}", receipt.revision);
    println!("  hash     {}", receipt.content_hash);
    println!("  nodes    {}", receipt.node_count);
    println!("  edges    {}", receipt.edge_count);
    println!("  roots    {}", receipt.roots.join(", ").if_empty("(none)"));
    println!("  ready    {}", receipt.ready.join(", ").if_empty("(none)"));
    println!(
        "  gates    {}",
        format_gates(&receipt.gates).if_empty("(none)")
    );
    Ok(())
}

/// Render gates as `id (kind)` pairs for compact text output.
#[must_use]
pub fn format_gates(gates: &[GraphFrontierGate]) -> String {
    gates
        .iter()
        .map(|gate| format!("{} ({})", gate.source_id, gate.kind))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Print a frontier as compact text or JSON.
///
/// # Errors
/// Returns JSON encode errors or large-stdout refusal.
pub fn print_frontier(
    frontier: &GraphFrontier,
    json: bool,
    confirm_large: bool,
) -> Result<(), VivariumError> {
    if json {
        return crate::stdout_budget::print_pretty_json(
            "graph ready",
            frontier,
            confirm_large,
            Some(&frontier.code),
        );
    }
    println!("graph {}", frontier.code);
    println!("  handle   {}", frontier.handle);
    println!("  revision {}", frontier.revision);
    println!(
        "  counts   ready={} blocked={} active={} awaiting={} reported={} gates={}",
        frontier.ready.len(),
        frontier.blocked.len(),
        frontier.active.len(),
        frontier.awaiting.len(),
        frontier.reported.len(),
        frontier.gates.len()
    );
    println!(
        "  ready    {}",
        frontier.ready.join(", ").if_empty("(none)")
    );
    println!(
        "  blocked  {}",
        frontier.blocked.join(", ").if_empty("(none)")
    );
    println!(
        "  active   {}",
        frontier.active.join(", ").if_empty("(none)")
    );
    println!(
        "  awaiting {}",
        frontier.awaiting.join(", ").if_empty("(none)")
    );
    println!(
        "  reported {}",
        frontier.reported.join(", ").if_empty("(none)")
    );
    println!(
        "  gates    {}",
        format_gates(&frontier.gates).if_empty("(none)")
    );
    Ok(())
}

/// Print blocked nodes as compact text or JSON.
///
/// # Errors
/// Returns JSON encode errors or large-stdout refusal.
pub fn print_blocked(
    nodes: &[GraphBlockedNode],
    json: bool,
    confirm_large: bool,
) -> Result<(), VivariumError> {
    if json {
        return crate::stdout_budget::print_pretty_json(
            "graph blocked",
            nodes,
            confirm_large,
            None,
        );
    }
    println!("blocked {}", nodes.len());
    if nodes.is_empty() {
        return Ok(());
    }
    println!("  source_id  kind  blocked_by  label");
    for node in nodes {
        println!(
            "  {}  {}  [{}]  {}",
            node.source_id,
            node.kind,
            node.blocked_by.join(", "),
            node.label
        );
    }
    Ok(())
}

/// Print a list of frontiers (all graphs).
///
/// # Errors
/// Returns JSON encode errors or large-stdout refusal.
pub fn print_frontiers(
    frontiers: &[GraphFrontier],
    json: bool,
    confirm_large: bool,
) -> Result<(), VivariumError> {
    if json {
        return crate::stdout_budget::print_pretty_json(
            "graph ready",
            frontiers,
            confirm_large,
            None,
        );
    }
    if frontiers.is_empty() {
        println!("no graphs");
        return Ok(());
    }
    for (i, frontier) in frontiers.iter().enumerate() {
        if i > 0 {
            println!();
        }
        print_frontier(frontier, false, confirm_large)?;
    }
    Ok(())
}

/// Print a lifecycle/mutation receipt as compact text or JSON.
///
/// # Errors
/// Returns JSON encode errors or large-stdout refusal.
pub fn print_action_receipt(
    receipt: &GraphActionReceipt,
    json: bool,
    confirm_large: bool,
) -> Result<(), VivariumError> {
    if json {
        return crate::stdout_budget::print_pretty_json(
            &format!("graph {}", receipt.action),
            receipt,
            confirm_large,
            Some(&receipt.code),
        );
    }
    println!("graph {}", receipt.action);
    println!("  code     {}", receipt.code);
    println!("  handle   {}", receipt.handle);
    println!("  revision {}", receipt.revision);
    if let Some(node) = &receipt.node {
        println!("  node     {node}");
    }
    if let Some(task) = &receipt.task {
        println!("  task     {task}");
    }
    if let Some(content) = &receipt.content {
        println!("  content  {content}");
    }
    println!("  ready    {}", receipt.ready.join(", ").if_empty("(none)"));
    println!(
        "  blocked  {}",
        receipt.blocked.join(", ").if_empty("(none)")
    );
    println!(
        "  active   {}",
        receipt.active.join(", ").if_empty("(none)")
    );
    println!(
        "  gates    {}",
        format_gates(&receipt.gates).if_empty("(none)")
    );
    Ok(())
}

fn source_ids(nodes: &[GraphNodeView]) -> Vec<String> {
    nodes.iter().map(|n| n.source_id.clone()).collect()
}

fn validate_code(code: &str) -> Result<(), VivariumError> {
    if code.is_empty() {
        return Err(VivariumError::Message(
            "graph code must not be empty".into(),
        ));
    }
    if !code
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '/')
    {
        return Err(VivariumError::Message(format!(
            "invalid graph code '{code}' (use [A-Za-z0-9_/-]+)"
        )));
    }
    Ok(())
}

fn compile_import(
    code: &str,
    mermaid_source: &str,
    content_hash: &str,
    flowchart: &MermaidFlowchart,
) -> WorkGraphImportInput {
    WorkGraphImportInput {
        code: code.to_string(),
        mermaid_source: mermaid_source.to_string(),
        content_hash: content_hash.to_string(),
        nodes: flowchart
            .nodes
            .iter()
            .map(|n| WorkGraphNodeInput {
                source_id: n.source_id.clone(),
                label: n.label.clone(),
                subgraph: n.subgraph.clone(),
                kind: n.kind.clone(),
            })
            .collect(),
        edges: flowchart
            .edges
            .iter()
            .map(|e| WorkGraphEdgeInput {
                from_source_id: e.from.clone(),
                to_source_id: e.to.clone(),
                label: e.label.clone(),
                style: e.style.clone(),
            })
            .collect(),
    }
}

fn preview_import(
    input: &WorkGraphImportInput,
    flowchart: &MermaidFlowchart,
    check_only: bool,
) -> GraphImportReport {
    let graph_handle = crate::storage::graph_handle_for_code(&input.code);
    let nodes: Vec<WorkGraphNodeRow> = flowchart
        .nodes
        .iter()
        .map(|n| WorkGraphNodeRow {
            handle: crate::storage::node_handle_for(&graph_handle, &n.source_id),
            graph_handle: graph_handle.clone(),
            source_id: n.source_id.clone(),
            label: n.label.clone(),
            state: "open".into(),
            subgraph: n.subgraph.clone(),
            kind: n.kind.clone(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .collect();
    let edges: Vec<WorkGraphEdgeRow> = flowchart
        .edges
        .iter()
        .map(|e| {
            let from_node = crate::storage::node_handle_for(&graph_handle, &e.from);
            let to_node = crate::storage::node_handle_for(&graph_handle, &e.to);
            WorkGraphEdgeRow {
                handle: crate::storage::edge_handle_for(&graph_handle, &from_node, &to_node),
                graph_handle: graph_handle.clone(),
                from_node,
                to_node,
                label: e.label.clone(),
                style: e.style.clone(),
                created_at: String::new(),
            }
        })
        .collect();
    let (node_views, ready, _) = project_nodes(&nodes, &edges);
    let edge_views = project_edges(&nodes, &edges);
    let roots: Vec<String> = ready.iter().map(|n| n.source_id.clone()).collect();
    GraphImportReport {
        check_only,
        created: false,
        idempotent: false,
        graph_handle,
        code: input.code.clone(),
        revision: 1,
        content_hash: input.content_hash.clone(),
        node_count: nodes.len(),
        edge_count: edges.len(),
        roots,
        ready,
        nodes: node_views,
        edges: edge_views,
    }
}

fn idempotent_or_conflict(
    storage: &Storage,
    existing: &WorkGraphRow,
    content_hash: &str,
    flowchart: &MermaidFlowchart,
    check_only: bool,
) -> Result<GraphImportReport, VivariumError> {
    let current_hash =
        storage.work_graph_revision_hash(&existing.handle, existing.current_revision)?;
    if current_hash.as_deref() == Some(content_hash) {
        let nodes = storage.work_graph_nodes(&existing.handle)?;
        let edges = storage.work_graph_edges(&existing.handle)?;
        let (node_views, ready, _) = project_nodes(&nodes, &edges);
        let edge_views = project_edges(&nodes, &edges);
        let roots: Vec<String> = ready.iter().map(|n| n.source_id.clone()).collect();
        return Ok(GraphImportReport {
            check_only,
            created: false,
            idempotent: true,
            graph_handle: existing.handle.clone(),
            code: existing.code.clone(),
            revision: existing.current_revision,
            content_hash: content_hash.to_string(),
            node_count: nodes.len(),
            edge_count: edges.len(),
            roots,
            ready,
            nodes: node_views,
            edges: edge_views,
        });
    }
    let _ = flowchart;
    Err(VivariumError::Message(format!(
        "graph code '{}' already exists (handle {}); use graph apply to revise",
        existing.code, existing.handle
    )))
}

fn report_from_commit(
    commit: &WorkGraphImportCommit,
    content_hash: &str,
    check_only: bool,
    idempotent: bool,
) -> GraphImportReport {
    let (node_views, ready, _) = project_nodes(&commit.nodes, &commit.edges);
    let edge_views = project_edges(&commit.nodes, &commit.edges);
    let roots: Vec<String> = ready.iter().map(|n| n.source_id.clone()).collect();
    GraphImportReport {
        check_only,
        created: commit.created,
        idempotent,
        graph_handle: commit.graph.handle.clone(),
        code: commit.graph.code.clone(),
        revision: commit.graph.current_revision,
        content_hash: content_hash.to_string(),
        node_count: commit.nodes.len(),
        edge_count: commit.edges.len(),
        roots,
        ready,
        nodes: node_views,
        edges: edge_views,
    }
}

fn resolve_graph(storage: &Storage, code_or_handle: &str) -> Result<WorkGraphRow, VivariumError> {
    if let Some(g) = storage.work_graph_by_code(code_or_handle)? {
        return Ok(g);
    }
    if let Some(g) = storage.work_graph_by_handle(code_or_handle)? {
        return Ok(g);
    }
    Err(VivariumError::Message(format!(
        "work graph not found: {code_or_handle}"
    )))
}

pub(super) fn project_nodes(
    nodes: &[WorkGraphNodeRow],
    edges: &[WorkGraphEdgeRow],
) -> (Vec<GraphNodeView>, Vec<GraphNodeView>, Vec<GraphNodeView>) {
    let by_handle: HashMap<&str, &WorkGraphNodeRow> =
        nodes.iter().map(|n| (n.handle.as_str(), n)).collect();
    let (prereqs, successors) = adjacency(edges);
    let mut views = Vec::with_capacity(nodes.len());
    let mut ready = Vec::new();
    let mut blocked = Vec::new();
    for node in nodes {
        let view = node_view(node, &by_handle, &prereqs, &successors);
        match view.readiness.as_str() {
            "ready" => ready.push(view.clone()),
            "blocked" => blocked.push(view.clone()),
            _ => {}
        }
        views.push(view);
    }
    views.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    ready.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    blocked.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    (views, ready, blocked)
}

type NodeAdj<'a> = HashMap<&'a str, Vec<&'a str>>;

/// Solid prerequisite adjacency (dotted couplings never gate readiness) and
/// full successor adjacency (all edges, informational).
fn adjacency(edges: &[WorkGraphEdgeRow]) -> (NodeAdj<'_>, NodeAdj<'_>) {
    let mut prereqs: NodeAdj<'_> = HashMap::new();
    let mut successors: NodeAdj<'_> = HashMap::new();
    for edge in edges {
        if edge.style != "dotted" {
            prereqs
                .entry(edge.to_node.as_str())
                .or_default()
                .push(edge.from_node.as_str());
        }
        successors
            .entry(edge.from_node.as_str())
            .or_default()
            .push(edge.to_node.as_str());
    }
    (prereqs, successors)
}

fn node_view(
    node: &WorkGraphNodeRow,
    by_handle: &HashMap<&str, &WorkGraphNodeRow>,
    prereqs: &NodeAdj<'_>,
    successors: &NodeAdj<'_>,
) -> GraphNodeView {
    let unfinished: Vec<String> = prereqs
        .get(node.handle.as_str())
        .into_iter()
        .flatten()
        .filter_map(|h| by_handle.get(h).copied())
        .filter(|p| p.state != "done")
        .map(|p| p.source_id.clone())
        .collect();
    let readiness = if node.state != "open" {
        "n/a".to_string()
    } else if unfinished.is_empty() {
        "ready".to_string()
    } else {
        "blocked".to_string()
    };
    let succ: Vec<String> = successors
        .get(node.handle.as_str())
        .into_iter()
        .flatten()
        .filter_map(|h| by_handle.get(h).map(|n| n.source_id.clone()))
        .collect();
    GraphNodeView {
        handle: node.handle.clone(),
        source_id: node.source_id.clone(),
        label: node.label.clone(),
        state: node.state.clone(),
        subgraph: node.subgraph.clone(),
        kind: node.kind.clone(),
        readiness,
        blocked_by: unfinished,
        successors: succ,
    }
}

pub(super) fn project_edges(
    nodes: &[WorkGraphNodeRow],
    edges: &[WorkGraphEdgeRow],
) -> Vec<GraphEdgeView> {
    let by_handle: HashMap<&str, &WorkGraphNodeRow> =
        nodes.iter().map(|n| (n.handle.as_str(), n)).collect();
    let mut out: Vec<GraphEdgeView> = edges
        .iter()
        .filter_map(|e| {
            let from = by_handle.get(e.from_node.as_str())?;
            let to = by_handle.get(e.to_node.as_str())?;
            Some(GraphEdgeView {
                handle: e.handle.clone(),
                from_handle: e.from_node.clone(),
                to_handle: e.to_node.clone(),
                from_source_id: from.source_id.clone(),
                to_source_id: to.source_id.clone(),
                label: e.label.clone(),
                style: e.style.clone(),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        (&a.from_source_id, &a.to_source_id).cmp(&(&b.from_source_id, &b.to_source_id))
    });
    out
}

trait IfEmpty {
    fn if_empty(self, fallback: &str) -> String;
}

impl IfEmpty for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}

#[cfg(test)]
#[path = "graph_test.rs"]
mod tests;
