//! Work-graph apply, append, complete, and Mermaid export.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use serde::Serialize;

use super::Mailspace;
use super::graph::{
    GraphEdgeView, GraphFrontierGate, GraphNodeView, GraphShow, project_edges, project_nodes,
    split_ready_gates,
};
use super::mermaid::{MermaidFlowchart, MermaidNode, is_gate_kind, parse_flowchart};
use crate::error::VivariumError;
use crate::storage::{
    Storage, WorkGraphActivateInput, WorkGraphApplyPlan, WorkGraphEdgeInput, WorkGraphEdgeRow,
    WorkGraphNodeInput, WorkGraphNodeRow, WorkGraphRow, sha256_hex,
};

/// Diff summary for apply / check.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphApplyReport {
    pub check_only: bool,
    pub idempotent: bool,
    pub graph_handle: String,
    pub code: String,
    pub revision: i64,
    pub content_hash: String,
    pub nodes_added: Vec<String>,
    pub nodes_updated: Vec<String>,
    pub nodes_removed: Vec<String>,
    pub edges_added: usize,
    pub edges_removed: usize,
    pub ready: Vec<GraphNodeView>,
    pub nodes: Vec<GraphNodeView>,
    pub edges: Vec<GraphEdgeView>,
}

impl Mailspace {
    /// Apply a Mermaid revision onto an existing graph.
    ///
    /// # Errors
    /// Returns parse, mutation-rule, or storage errors.
    pub fn graph_apply(
        &self,
        code_or_handle: &str,
        mermaid_source: &str,
        check_only: bool,
    ) -> Result<GraphApplyReport, VivariumError> {
        let flowchart = parse_flowchart(mermaid_source)?;
        let content_hash = sha256_hex(mermaid_source.as_bytes());
        let mut storage = self.storage()?;
        let graph = resolve_graph(&storage, code_or_handle)?;
        let current_hash =
            storage.work_graph_revision_hash(&graph.handle, graph.current_revision)?;
        if current_hash.as_deref() == Some(content_hash.as_str()) {
            return idempotent_apply_report(&storage, &graph, &content_hash, check_only);
        }
        let existing_nodes = storage.work_graph_nodes(&graph.handle)?;
        let existing_edges = storage.work_graph_edges(&graph.handle)?;
        let plan = build_apply_plan(
            &graph,
            mermaid_source,
            &content_hash,
            &flowchart,
            &existing_nodes,
            &existing_edges,
        )?;
        if check_only {
            return Ok(report_from_plan(
                &plan,
                &existing_nodes,
                &existing_edges,
                true,
            ));
        }
        let graph = storage.apply_work_graph_plan(&plan)?;
        let nodes = storage.work_graph_nodes(&graph.handle)?;
        let edges = storage.work_graph_edges(&graph.handle)?;
        Ok(report_after_apply(&plan, &graph, &nodes, &edges, false))
    }

    /// Apply Mermaid from a file onto an existing graph.
    ///
    /// # Errors
    /// Returns IO, parse, or storage errors.
    pub fn graph_apply_file(
        &self,
        code_or_handle: &str,
        path: &Path,
        check_only: bool,
    ) -> Result<GraphApplyReport, VivariumError> {
        let source = std::fs::read_to_string(path).map_err(|e| {
            VivariumError::Other(format!("failed to read graph file {}: {e}", path.display()))
        })?;
        self.graph_apply(code_or_handle, &source, check_only)
    }

    /// Append one open node, optionally as an operator gate.
    ///
    /// # Errors
    /// Returns validation or storage errors.
    pub fn graph_node_add(
        &self,
        code_or_handle: &str,
        source_id: &str,
        label: Option<&str>,
        kind: Option<&str>,
    ) -> Result<GraphShow, VivariumError> {
        validate_source_id(source_id)?;
        validate_node_kind(kind)?;
        let show = self.graph_show(code_or_handle)?;
        let merged = merge_export_with_extra_node(
            &show,
            source_id,
            label.unwrap_or(source_id),
            kind.unwrap_or("task"),
        )?;
        self.graph_apply(code_or_handle, &merged, false)?;
        self.graph_show(code_or_handle)
    }

    /// Append one dependency edge.
    ///
    /// # Errors
    /// Returns validation or storage errors.
    pub fn graph_edge_add(
        &self,
        code_or_handle: &str,
        from: &str,
        to: &str,
        label: Option<&str>,
    ) -> Result<GraphShow, VivariumError> {
        validate_source_id(from)?;
        validate_source_id(to)?;
        let show = self.graph_show(code_or_handle)?;
        let merged = merge_export_with_extra_edge(&show, from, to, label)?;
        self.graph_apply(code_or_handle, &merged, false)?;
        self.graph_show(code_or_handle)
    }

    /// Mark a node done and refresh readiness (no agent spawn).
    ///
    /// # Errors
    /// Returns not-found or storage errors.
    pub fn graph_complete(
        &self,
        code_or_handle: &str,
        source_id: &str,
        note: Option<&str>,
    ) -> Result<GraphShow, VivariumError> {
        let mut storage = self.storage()?;
        let graph = resolve_graph(&storage, code_or_handle)?;
        let rows = storage.work_graph_nodes(&graph.handle)?;
        let edges = storage.work_graph_edges(&graph.handle)?;
        let target = rows
            .iter()
            .find(|n| n.source_id == source_id)
            .ok_or_else(|| node_not_found_error(&storage, source_id))?;
        if target.state == "done" {
            return self.graph_show(code_or_handle);
        }
        if matches!(target.state.as_str(), "cancelled" | "superseded") {
            return Err(VivariumError::Message(format!(
                "cannot complete node in state '{}'",
                target.state
            )));
        }
        let ready_before = ready_handles(&rows, &edges);
        let newly_ready = newly_ready_after_done(&rows, &edges, &target.handle, &ready_before);
        storage.complete_work_graph_node(
            &graph.handle,
            &target.handle,
            note,
            &newly_ready,
            Some("via=graph-complete"),
        )?;
        self.graph_show(code_or_handle)
    }

    /// Bind a task attempt and mark a ready open node active.
    ///
    /// # Errors
    /// Returns validation or storage errors.
    pub fn graph_activate(
        &self,
        code_or_handle: &str,
        source_id: &str,
        task_token: &str,
        note: Option<&str>,
    ) -> Result<GraphShow, VivariumError> {
        let mut storage = self.storage()?;
        let graph = resolve_graph(&storage, code_or_handle)?;
        let rows = storage.work_graph_nodes(&graph.handle)?;
        let edges = storage.work_graph_edges(&graph.handle)?;
        let target = rows
            .iter()
            .find(|n| n.source_id == source_id)
            .ok_or_else(|| node_not_found_error(&storage, source_id))?;
        if target.state != "open" {
            return Err(VivariumError::Message(format!(
                "cannot activate node '{}' in state '{}'",
                source_id, target.state
            )));
        }
        if is_gate_kind(&target.kind) {
            return Err(VivariumError::Message(format!(
                "cannot activate {} node '{}'; operator-gated nodes are resolved with \
                 graph complete --note, not dispatched",
                target.kind, source_id
            )));
        }
        self.refuse_unowned_activation(&storage, &graph.code, source_id)?;
        self.refuse_reported_activation(&storage, &graph.code, source_id)?;
        let (_, ready, _) = project_nodes(&rows, &edges);
        if !ready.iter().any(|n| n.handle == target.handle) {
            return Err(VivariumError::Message(format!(
                "cannot activate blocked node '{source_id}'"
            )));
        }
        let task_message_id = storage.resolve_message_token(task_token)?;
        let task_handle = storage.display_handle(&task_message_id)?;
        let task = storage.message_by_id(&task_message_id)?.ok_or_else(|| {
            VivariumError::Message(format!("task message not found: {task_token}"))
        })?;
        if task.local_role != "tasks" {
            return Err(VivariumError::Message(format!(
                "activate requires a task handle; got role '{}'",
                task.local_role
            )));
        }
        storage.activate_work_graph_node(&WorkGraphActivateInput {
            graph_handle: graph.handle.clone(),
            node_handle: target.handle.clone(),
            task_message_id,
            task_handle,
            task_content_hash: task.content_id.clone(),
            role: Some(task.account.clone()),
            note: note.map(str::to_string),
        })?;
        self.graph_show(code_or_handle)
    }

    /// Content hash (sha256 of the stored record) behind a handle or token —
    /// the citable pin for "record as sent/dispatched".
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the token does not resolve.
    pub fn content_hash_of(&self, token: &str) -> Result<String, VivariumError> {
        let storage = self.storage()?;
        let message_id = storage.resolve_message_token(token)?;
        let message = storage
            .message_by_id(&message_id)?
            .ok_or_else(|| VivariumError::Message(format!("message not found: {token}")))?;
        Ok(message.content_id)
    }

    /// Summaries of all graphs for board projection.
    ///
    /// # Errors
    /// Returns storage errors.
    pub fn graph_board_summaries(&self) -> Result<Vec<GraphShow>, VivariumError> {
        let storage = self.storage()?;
        let graphs = storage.work_graphs()?;
        let mut out = Vec::with_capacity(graphs.len());
        for graph in graphs {
            out.push(self.graph_show(&graph.code)?);
        }
        Ok(out)
    }

    /// Export normalized Mermaid (optional state classes).
    ///
    /// # Errors
    /// Returns not-found errors.
    pub fn graph_export_mermaid(
        &self,
        code_or_handle: &str,
        include_state: bool,
    ) -> Result<String, VivariumError> {
        let show = self.graph_show(code_or_handle)?;
        Ok(export_mermaid(&show, include_state))
    }
}

/// Compact apply receipt for CLI (no full node/edge topology).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphApplyReceipt {
    pub check_only: bool,
    pub idempotent: bool,
    pub graph_handle: String,
    pub code: String,
    pub revision: i64,
    pub content_hash: String,
    pub nodes_added: Vec<String>,
    pub nodes_updated: Vec<String>,
    pub nodes_removed: Vec<String>,
    pub edges_added: usize,
    pub edges_removed: usize,
    pub ready: Vec<String>,
    pub gates: Vec<GraphFrontierGate>,
}

/// Print apply report as compact text or JSON receipt.
///
/// # Errors
/// Returns JSON encode errors or large-stdout refusal.
pub fn print_apply_report(
    report: &GraphApplyReport,
    json: bool,
    confirm_large: bool,
) -> Result<(), VivariumError> {
    let (ready, gates) = split_ready_gates(&report.ready);
    let receipt = GraphApplyReceipt {
        check_only: report.check_only,
        idempotent: report.idempotent,
        graph_handle: report.graph_handle.clone(),
        code: report.code.clone(),
        revision: report.revision,
        content_hash: report.content_hash.clone(),
        nodes_added: report.nodes_added.clone(),
        nodes_updated: report.nodes_updated.clone(),
        nodes_removed: report.nodes_removed.clone(),
        edges_added: report.edges_added,
        edges_removed: report.edges_removed,
        ready,
        gates,
    };
    if json {
        return crate::stdout_budget::print_pretty_json(
            "graph apply",
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
        "applied"
    };
    println!("graph {mode}");
    println!("  handle   {}", receipt.graph_handle);
    println!("  code     {}", receipt.code);
    println!("  revision {}", receipt.revision);
    println!("  +nodes   {}", receipt.nodes_added.join(", "));
    println!("  ~nodes   {}", receipt.nodes_updated.join(", "));
    println!("  -nodes   {}", receipt.nodes_removed.join(", "));
    println!("  +edges   {}", receipt.edges_added);
    println!("  -edges   {}", receipt.edges_removed);
    let ready = if receipt.ready.is_empty() {
        "(none)".to_string()
    } else {
        receipt.ready.join(", ")
    };
    println!("  ready    {ready}");
    println!("  gates    {}", super::graph::format_gates(&receipt.gates));
    Ok(())
}

fn build_apply_plan(
    graph: &WorkGraphRow,
    mermaid_source: &str,
    content_hash: &str,
    flowchart: &MermaidFlowchart,
    existing_nodes: &[WorkGraphNodeRow],
    existing_edges: &[WorkGraphEdgeRow],
) -> Result<WorkGraphApplyPlan, VivariumError> {
    let by_source: HashMap<&str, &WorkGraphNodeRow> = existing_nodes
        .iter()
        .map(|n| (n.source_id.as_str(), n))
        .collect();
    let (nodes_add, nodes_update) = plan_node_upserts(flowchart, &by_source)?;
    let nodes_remove = plan_node_removes(flowchart, existing_nodes)?;
    let (edges_add, edges_remove) =
        plan_edge_diff(flowchart, existing_nodes, existing_edges, &by_source)?;
    let next_revision = graph.current_revision + 1;
    let event_note = format!(
        "revision={next_revision} +n={} ~n={} -n={} +e={} -e={}",
        nodes_add.len(),
        nodes_update.len(),
        nodes_remove.len(),
        edges_add.len(),
        edges_remove.len()
    );
    Ok(WorkGraphApplyPlan {
        graph_handle: graph.handle.clone(),
        code: graph.code.clone(),
        next_revision,
        mermaid_source: mermaid_source.to_string(),
        content_hash: content_hash.to_string(),
        nodes_add,
        nodes_update,
        nodes_remove,
        edges_add,
        edges_remove,
        event_note,
    })
}

fn plan_node_upserts(
    flowchart: &MermaidFlowchart,
    by_source: &HashMap<&str, &WorkGraphNodeRow>,
) -> Result<(Vec<WorkGraphNodeInput>, Vec<WorkGraphNodeInput>), VivariumError> {
    let mut nodes_add = Vec::new();
    let mut nodes_update = Vec::new();
    for node in &flowchart.nodes {
        match by_source.get(node.source_id.as_str()) {
            None => nodes_add.push(WorkGraphNodeInput {
                source_id: node.source_id.clone(),
                label: node.label.clone(),
                subgraph: node.subgraph.clone(),
                kind: node.kind.clone(),
            }),
            Some(existing) => plan_node_update(existing, node, &mut nodes_update)?,
        }
    }
    Ok((nodes_add, nodes_update))
}

fn plan_node_removes(
    flowchart: &MermaidFlowchart,
    existing_nodes: &[WorkGraphNodeRow],
) -> Result<Vec<String>, VivariumError> {
    let desired_ids: HashSet<&str> = flowchart
        .nodes
        .iter()
        .map(|n| n.source_id.as_str())
        .collect();
    let mut nodes_remove = Vec::new();
    for existing in existing_nodes {
        if desired_ids.contains(existing.source_id.as_str()) {
            continue;
        }
        if existing.state != "open" {
            return Err(VivariumError::Message(format!(
                "cannot remove node '{}' in state '{}'",
                existing.source_id, existing.state
            )));
        }
        nodes_remove.push(existing.source_id.clone());
    }
    Ok(nodes_remove)
}

fn plan_node_update(
    existing: &WorkGraphNodeRow,
    desired: &MermaidNode,
    nodes_update: &mut Vec<WorkGraphNodeInput>,
) -> Result<(), VivariumError> {
    let label_changed = existing.label != desired.label;
    let sub_changed = existing.subgraph != desired.subgraph;
    let kind_changed = existing.kind != desired.kind;
    if !label_changed && !sub_changed && !kind_changed {
        return Ok(());
    }
    if existing.state != "open" {
        return Err(VivariumError::Message(format!(
            "cannot edit node '{}' in state '{}'",
            existing.source_id, existing.state
        )));
    }
    nodes_update.push(WorkGraphNodeInput {
        source_id: desired.source_id.clone(),
        label: desired.label.clone(),
        subgraph: desired.subgraph.clone(),
        kind: desired.kind.clone(),
    });
    Ok(())
}

/// Edge diff key: endpoints plus style, so flipping a coupling between
/// solid and dotted revises the edge instead of silently keeping the old one.
type EdgeKey = (String, String, String);
type EdgeDiff = (Vec<WorkGraphEdgeInput>, Vec<(String, String)>);

fn plan_edge_diff(
    flowchart: &MermaidFlowchart,
    existing_nodes: &[WorkGraphNodeRow],
    existing_edges: &[WorkGraphEdgeRow],
    by_source: &HashMap<&str, &WorkGraphNodeRow>,
) -> Result<EdgeDiff, VivariumError> {
    let existing_keys = existing_edge_keys(existing_nodes, existing_edges);
    let desired_keys: HashSet<EdgeKey> = flowchart
        .edges
        .iter()
        .map(|e| (e.from.clone(), e.to.clone(), e.style.clone()))
        .collect();
    let edges_add = plan_edges_add(flowchart, &existing_keys, by_source)?;
    let edges_remove = plan_edges_remove(&existing_keys, &desired_keys, by_source)?;
    Ok((edges_add, edges_remove))
}

fn existing_edge_keys(
    existing_nodes: &[WorkGraphNodeRow],
    existing_edges: &[WorkGraphEdgeRow],
) -> HashSet<EdgeKey> {
    let handle_to_source: HashMap<&str, &str> = existing_nodes
        .iter()
        .map(|n| (n.handle.as_str(), n.source_id.as_str()))
        .collect();
    existing_edges
        .iter()
        .filter_map(|e| {
            let from = handle_to_source.get(e.from_node.as_str())?;
            let to = handle_to_source.get(e.to_node.as_str())?;
            Some(((*from).to_string(), (*to).to_string(), e.style.clone()))
        })
        .collect()
}

fn plan_edges_add(
    flowchart: &MermaidFlowchart,
    existing_keys: &HashSet<EdgeKey>,
    by_source: &HashMap<&str, &WorkGraphNodeRow>,
) -> Result<Vec<WorkGraphEdgeInput>, VivariumError> {
    let mut edges_add = Vec::new();
    for edge in &flowchart.edges {
        let key = (edge.from.clone(), edge.to.clone(), edge.style.clone());
        if existing_keys.contains(&key) {
            continue;
        }
        if let Some(target) = by_source
            .get(edge.to.as_str())
            .filter(|t| is_frozen_target(t.state.as_str()))
        {
            return Err(VivariumError::Message(format!(
                "cannot add prerequisite to {} node '{}'",
                target.state, edge.to
            )));
        }
        edges_add.push(WorkGraphEdgeInput {
            from_source_id: edge.from.clone(),
            to_source_id: edge.to.clone(),
            label: edge.label.clone(),
            style: edge.style.clone(),
        });
    }
    Ok(edges_add)
}

fn plan_edges_remove(
    existing_keys: &HashSet<EdgeKey>,
    desired_keys: &HashSet<EdgeKey>,
    by_source: &HashMap<&str, &WorkGraphNodeRow>,
) -> Result<Vec<(String, String)>, VivariumError> {
    let mut edges_remove = Vec::new();
    for (from, to, style) in existing_keys {
        if desired_keys.contains(&(from.clone(), to.clone(), style.clone())) {
            continue;
        }
        if let Some(target) = by_source
            .get(to.as_str())
            .filter(|t| is_frozen_target(t.state.as_str()))
        {
            return Err(VivariumError::Message(format!(
                "cannot remove prerequisite of {} node '{}'",
                target.state, to
            )));
        }
        edges_remove.push((from.clone(), to.clone()));
    }
    Ok(edges_remove)
}

fn is_frozen_target(state: &str) -> bool {
    matches!(state, "active" | "done")
}

fn idempotent_apply_report(
    storage: &Storage,
    graph: &WorkGraphRow,
    content_hash: &str,
    check_only: bool,
) -> Result<GraphApplyReport, VivariumError> {
    let nodes = storage.work_graph_nodes(&graph.handle)?;
    let edges = storage.work_graph_edges(&graph.handle)?;
    let (node_views, ready, _) = project_nodes(&nodes, &edges);
    let edge_views = project_edges(&nodes, &edges);
    Ok(GraphApplyReport {
        check_only,
        idempotent: true,
        graph_handle: graph.handle.clone(),
        code: graph.code.clone(),
        revision: graph.current_revision,
        content_hash: content_hash.to_string(),
        nodes_added: Vec::new(),
        nodes_updated: Vec::new(),
        nodes_removed: Vec::new(),
        edges_added: 0,
        edges_removed: 0,
        ready,
        nodes: node_views,
        edges: edge_views,
    })
}

fn report_from_plan(
    plan: &WorkGraphApplyPlan,
    existing_nodes: &[WorkGraphNodeRow],
    existing_edges: &[WorkGraphEdgeRow],
    check_only: bool,
) -> GraphApplyReport {
    // Preview uses existing topology plus planned adds for ready projection of current state.
    let (node_views, ready, _) = project_nodes(existing_nodes, existing_edges);
    let edge_views = project_edges(existing_nodes, existing_edges);
    GraphApplyReport {
        check_only,
        idempotent: false,
        graph_handle: plan.graph_handle.clone(),
        code: plan.code.clone(),
        revision: plan.next_revision,
        content_hash: plan.content_hash.clone(),
        nodes_added: plan.nodes_add.iter().map(|n| n.source_id.clone()).collect(),
        nodes_updated: plan
            .nodes_update
            .iter()
            .map(|n| n.source_id.clone())
            .collect(),
        nodes_removed: plan.nodes_remove.clone(),
        edges_added: plan.edges_add.len(),
        edges_removed: plan.edges_remove.len(),
        ready,
        nodes: node_views,
        edges: edge_views,
    }
}

fn report_after_apply(
    plan: &WorkGraphApplyPlan,
    graph: &WorkGraphRow,
    nodes: &[WorkGraphNodeRow],
    edges: &[WorkGraphEdgeRow],
    check_only: bool,
) -> GraphApplyReport {
    let (node_views, ready, _) = project_nodes(nodes, edges);
    let edge_views = project_edges(nodes, edges);
    GraphApplyReport {
        check_only,
        idempotent: false,
        graph_handle: graph.handle.clone(),
        code: graph.code.clone(),
        revision: graph.current_revision,
        content_hash: plan.content_hash.clone(),
        nodes_added: plan.nodes_add.iter().map(|n| n.source_id.clone()).collect(),
        nodes_updated: plan
            .nodes_update
            .iter()
            .map(|n| n.source_id.clone())
            .collect(),
        nodes_removed: plan.nodes_remove.clone(),
        edges_added: plan.edges_add.len(),
        edges_removed: plan.edges_remove.len(),
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

/// Not-found error for a node source id, pointing at the audit repair path
/// when the id resolves to a real work message that simply lacks a node.
fn node_not_found_error(storage: &Storage, source_id: &str) -> VivariumError {
    let mut message = format!("graph node source id not found: {source_id}");
    if let Ok(message_id) = storage.resolve_message_token(source_id)
        && let Ok(Some(view)) = storage.message_by_id(&message_id)
        && matches!(
            view.local_role.as_str(),
            "tasks" | "needs" | "wants" | "done"
        )
    {
        message.push_str("; work item has no backlog node (vivi graph audit --repair recovers it)");
    }
    VivariumError::Message(message)
}

pub(super) fn ready_handles(
    nodes: &[WorkGraphNodeRow],
    edges: &[WorkGraphEdgeRow],
) -> HashSet<String> {
    let (_, ready, _) = project_nodes(nodes, edges);
    ready.into_iter().map(|n| n.handle).collect()
}

pub(super) fn newly_ready_after_done(
    nodes: &[WorkGraphNodeRow],
    edges: &[WorkGraphEdgeRow],
    completed_handle: &str,
    ready_before: &HashSet<String>,
) -> Vec<String> {
    let mut projected: Vec<WorkGraphNodeRow> = nodes.to_vec();
    for node in &mut projected {
        if node.handle == completed_handle {
            node.state = "done".into();
        }
    }
    let ready_after = ready_handles(&projected, edges);
    ready_after
        .into_iter()
        .filter(|h| !ready_before.contains(h) && h != completed_handle)
        .collect()
}

pub(super) fn validate_source_id(id: &str) -> Result<(), VivariumError> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(VivariumError::Message(format!(
            "invalid source id '{id}' (use [A-Za-z0-9_-]+)"
        )));
    }
    Ok(())
}

fn validate_node_kind(kind: Option<&str>) -> Result<(), VivariumError> {
    let Some(kind) = kind else {
        return Ok(());
    };
    if kind == "task" || is_gate_kind(kind) {
        Ok(())
    } else {
        Err(VivariumError::Message(format!(
            "invalid node kind '{kind}' (use task, decision, stub, or parked)"
        )))
    }
}

fn escape_label(label: &str) -> String {
    label.replace('"', "'")
}

pub(super) fn export_mermaid(show: &GraphShow, include_state: bool) -> String {
    let mut out = String::from("flowchart LR\n");
    for node in &show.nodes {
        let label = escape_label(&node.label);
        match node.kind.as_str() {
            "decision" => {
                let _ = writeln!(out, "  {}{{\"{label}\"}}", node.source_id);
            }
            "stub" | "parked" => {
                let _ = writeln!(out, "  {}[\"{label}\"]:::{}", node.source_id, node.kind);
            }
            _ => {
                let _ = writeln!(out, "  {}[\"{label}\"]", node.source_id);
            }
        }
    }
    for edge in &show.edges {
        let arrow = if edge.style == "dotted" {
            "-.->"
        } else {
            "-->"
        };
        if let Some(label) = &edge.label {
            let _ = writeln!(
                out,
                "  {} {arrow}|{}| {}",
                edge.from_source_id,
                escape_label(label),
                edge.to_source_id
            );
        } else {
            let _ = writeln!(
                out,
                "  {} {arrow} {}",
                edge.from_source_id, edge.to_source_id
            );
        }
    }
    if include_state {
        out.push_str("  classDef done fill:#cfc,stroke:#393;\n");
        out.push_str("  classDef ready fill:#cef,stroke:#369;\n");
        out.push_str("  classDef blocked fill:#fec,stroke:#a60;\n");
        for node in &show.nodes {
            let class = if node.state == "done" {
                "done"
            } else if node.readiness == "ready" {
                "ready"
            } else if node.readiness == "blocked" {
                "blocked"
            } else {
                continue;
            };
            let _ = writeln!(out, "  class {} {class}", node.source_id);
        }
    }
    out
}

fn merge_export_with_extra_node(
    show: &GraphShow,
    source_id: &str,
    label: &str,
    kind: &str,
) -> Result<String, VivariumError> {
    if show.nodes.iter().any(|n| n.source_id == source_id) {
        return Err(VivariumError::Message(format!(
            "node source id already exists: {source_id}"
        )));
    }
    let mut nodes = show.nodes.clone();
    nodes.push(GraphNodeView {
        handle: String::new(),
        source_id: source_id.to_string(),
        label: label.to_string(),
        state: "open".into(),
        subgraph: None,
        kind: kind.into(),
        readiness: "ready".into(),
        blocked_by: Vec::new(),
        successors: Vec::new(),
    });
    let synthetic = GraphShow {
        nodes,
        edges: show.edges.clone(),
        ..show.clone()
    };
    Ok(export_mermaid(&synthetic, false))
}

fn merge_export_with_extra_edge(
    show: &GraphShow,
    from: &str,
    to: &str,
    label: Option<&str>,
) -> Result<String, VivariumError> {
    if !show.nodes.iter().any(|n| n.source_id == from) {
        return Err(VivariumError::Message(format!(
            "edge from unknown node '{from}'"
        )));
    }
    if !show.nodes.iter().any(|n| n.source_id == to) {
        return Err(VivariumError::Message(format!(
            "edge to unknown node '{to}'"
        )));
    }
    if show
        .edges
        .iter()
        .any(|e| e.from_source_id == from && e.to_source_id == to)
    {
        return Err(VivariumError::Message(format!(
            "edge already exists: {from} --> {to}"
        )));
    }
    let mut edges = show.edges.clone();
    edges.push(GraphEdgeView {
        handle: String::new(),
        from_handle: String::new(),
        to_handle: String::new(),
        from_source_id: from.to_string(),
        to_source_id: to.to_string(),
        label: label.map(str::to_string),
        style: "solid".into(),
    });
    let synthetic = GraphShow {
        edges,
        nodes: show.nodes.clone(),
        ..show.clone()
    };
    Ok(export_mermaid(&synthetic, false))
}

#[cfg(test)]
#[path = "graph_mutate_test.rs"]
mod tests;
