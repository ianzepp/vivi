//! Graph command runners: import/apply/show/export, frontier reads with
//! kind filtering, backlog audit/repair, connect, complete, activate, and
//! node/edge appends.

use std::collections::HashSet;

use vivi::VivariumError;
use vivi::cli::{
    GraphActivateCommand, GraphApplyCommand, GraphAuditCommand, GraphCommand, GraphCompleteCommand,
    GraphConnectCommand, GraphExportCommand, GraphImportCommand, GraphNodeCommand,
    GraphReadyCommand, GraphShowCommand,
};
use vivi::mailspace::{GraphFrontier, GraphShow, Mailspace, frontier_from_show};

pub(crate) fn handle_graph_command(command: &GraphCommand) -> Result<(), VivariumError> {
    match command {
        GraphCommand::Import(command) => handle_graph_import(command),
        GraphCommand::Apply(command) => handle_graph_apply(command),
        GraphCommand::Show(command) => handle_graph_show(command),
        GraphCommand::Export(command) => handle_graph_export(command),
        GraphCommand::Ready(command) => handle_graph_ready(command),
        GraphCommand::Audit(command) => handle_graph_audit(command),
        GraphCommand::Connect(command) => handle_graph_connect(command),
        GraphCommand::Complete(command) => handle_graph_complete(command),
        GraphCommand::Activate(command) => handle_graph_activate(command),
        GraphCommand::Node { command } => handle_graph_node_command(command),
        GraphCommand::Edge { command } => handle_graph_edge_command(command),
    }
}

fn handle_graph_import(command: &GraphImportCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let report = mailspace.graph_import_file(&command.code, &command.file, command.check)?;
    vivi::mailspace::print_import_report(&report, command.json, command.confirm_large)
}

fn handle_graph_apply(command: &GraphApplyCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let report = mailspace.graph_apply_file(&command.graph, &command.file, command.check)?;
    vivi::mailspace::print_apply_report(&report, command.json, command.confirm_large)
}

fn handle_graph_show(command: &GraphShowCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let mermaid = mailspace.graph_export_mermaid(&command.graph, command.include_state)?;
    print!("{mermaid}");
    Ok(())
}

fn handle_graph_export(command: &GraphExportCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let mermaid = mailspace.graph_export_mermaid(&command.graph, command.include_state)?;
    print!("{mermaid}");
    Ok(())
}

fn handle_graph_ready(command: &GraphReadyCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    if let Some(kind) = command.kind.as_deref() {
        validate_kind_filter(kind)?;
    }
    if let Some(graph) = command.graph.as_deref() {
        let show = mailspace.graph_show(graph)?;
        let frontier = frontier_with_kind_filter(&show, command.kind.as_deref());
        return vivi::mailspace::print_frontier(&frontier, command.json, command.confirm_large);
    }
    let shows = mailspace.graph_board_summaries()?;
    let frontiers: Vec<_> = shows
        .iter()
        .map(|show| frontier_with_kind_filter(show, command.kind.as_deref()))
        .collect();
    vivi::mailspace::print_frontiers(&frontiers, command.json, command.confirm_large)
}

fn validate_kind_filter(kind: &str) -> Result<(), VivariumError> {
    let known = ["task", "need", "want", "decision", "stub", "parked"];
    if known.contains(&kind) {
        return Ok(());
    }
    Err(VivariumError::Message(format!(
        "unknown kind '{kind}' (use {})",
        known.join(", ")
    )))
}

/// Frontier projection keeping only nodes of one kind, so large backlogs
/// stay readable in status loops.
fn frontier_with_kind_filter(show: &GraphShow, kind: Option<&str>) -> GraphFrontier {
    let mut frontier = frontier_from_show(show);
    let Some(kind) = kind else {
        return frontier;
    };
    let matching: HashSet<String> = show
        .nodes
        .iter()
        .filter(|node| node.kind == kind)
        .map(|node| node.source_id.clone())
        .collect();
    frontier.ready.retain(|id| matching.contains(id));
    frontier.blocked.retain(|id| matching.contains(id));
    frontier.active.retain(|id| matching.contains(id));
    frontier.awaiting.retain(|id| matching.contains(id));
    frontier.gates.retain(|gate| gate.kind == kind);
    frontier
}

fn handle_graph_audit(command: &GraphAuditCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let report = mailspace.backlog_audit(command.repair)?;
    vivi::mailspace::print_backlog_audit(&report, command.json, command.confirm_large)
}

fn handle_graph_connect(command: &GraphConnectCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    mailspace.backlog_connect(
        &command.dependent,
        &command.prereq,
        command.label.as_deref(),
    )?;
    if command.json {
        let show = mailspace.graph_show("backlog")?;
        let frontier = frontier_from_show(&show);
        return vivi::mailspace::print_frontier(&frontier, true, command.confirm_large);
    }
    println!("connected {} -> {}", command.prereq, command.dependent);
    let show = mailspace.graph_show("backlog")?;
    let frontier = frontier_from_show(&show);
    vivi::mailspace::print_frontier(&frontier, false, command.confirm_large)
}

fn handle_graph_complete(command: &GraphCompleteCommand) -> Result<(), VivariumError> {
    if command.task.is_some() {
        eprintln!("note: --task is ignored by complete; task binding happens at graph activate");
    }
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let (graph, source_id) = split_graph_node(&command.node, command.graph.as_deref())?;
    let show = mailspace.graph_complete(&graph, &source_id, command.note.as_deref())?;
    let receipt =
        vivi::mailspace::action_receipt_from_show("complete", &show, Some(&source_id), None);
    vivi::mailspace::print_action_receipt(&receipt, command.json, command.confirm_large)
}

fn handle_graph_activate(command: &GraphActivateCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let (graph, source_id) = split_graph_node(&command.node, command.graph.as_deref())?;
    let show =
        mailspace.graph_activate(&graph, &source_id, &command.task, command.note.as_deref())?;
    let mut receipt = vivi::mailspace::action_receipt_from_show(
        "activate",
        &show,
        Some(&source_id),
        Some(&command.task),
    );
    receipt.content = mailspace.content_hash_of(&command.task).ok();
    vivi::mailspace::print_action_receipt(&receipt, command.json, command.confirm_large)
}

fn handle_graph_node_command(command: &GraphNodeCommand) -> Result<(), VivariumError> {
    let GraphNodeCommand::Add(command) = command;
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let show = mailspace.graph_node_add(
        &command.graph,
        &command.id,
        command.label.as_deref(),
        command.kind.as_deref(),
    )?;
    let receipt =
        vivi::mailspace::action_receipt_from_show("node_add", &show, Some(&command.id), None);
    vivi::mailspace::print_action_receipt(&receipt, command.json, command.confirm_large)
}

fn handle_graph_edge_command(command: &vivi::cli::GraphEdgeCommand) -> Result<(), VivariumError> {
    let vivi::cli::GraphEdgeCommand::Add(command) = command;
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let show = mailspace.graph_edge_add(
        &command.graph,
        &command.from,
        &command.to,
        command.label.as_deref(),
    )?;
    let node = format!("{}->{}", command.from, command.to);
    let receipt = vivi::mailspace::action_receipt_from_show("edge_add", &show, Some(&node), None);
    vivi::mailspace::print_action_receipt(&receipt, command.json, command.confirm_large)
}

fn split_graph_node(
    node: &str,
    graph_flag: Option<&str>,
) -> Result<(String, String), VivariumError> {
    if let Some((graph, source_id)) = node.split_once(':')
        && !graph.is_empty()
        && !source_id.is_empty()
    {
        return Ok((graph.to_string(), source_id.to_string()));
    }
    // Bare source ids address the backlog graph: its ids are mailspace
    // handles, so the common dispatch path needs no prefix.
    let graph = graph_flag
        .map(str::to_string)
        .unwrap_or_else(|| "backlog".to_string());
    Ok((graph, node.to_string()))
}
