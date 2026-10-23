//! Backlog citizenship audit: verify every work-kind message that existed
//! while the backlog graph did has a node in step with its folder state,
//! and optionally repair drift. Answers "why does `graph activate` /
//! `need bind` reject this handle" in one command — including untracked
//! items sent by older vivi binaries that did not mint nodes.

use std::collections::HashSet;

use serde::Serialize;

use super::Mailspace;
use super::backlog::backlog_node_by_token;
use super::holding::is_foreign_account;
use super::item_events::event_release_due;
use super::want_state::WantState;
use crate::error::VivariumError;
use crate::storage::{
    BACKLOG_GRAPH_CODE, Storage, StoredMessageView, WorkGraphEdgeRow, WorkGraphNodeRow,
};

/// One citizenship violation.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BacklogAuditFinding {
    pub class: String,
    pub kind: String,
    pub handle: String,
    pub subject: String,
    pub detail: String,
    pub repaired: bool,
}

/// Full audit result. `checked` counts candidate messages (content-deduped
/// copies are audited once).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BacklogAuditReport {
    pub graph: String,
    pub checked: usize,
    pub findings: Vec<BacklogAuditFinding>,
}

/// The canonical copy of one work item (multi-recipient sends deliver
/// per-identity copies of one content).
struct AuditItem {
    handle: String,
    kind: String,
    folder_done: bool,
    /// Whether the node should be done: the folder for a task or need, the
    /// computed state for a want (a want has no folder move of its own).
    done: bool,
    subject: String,
    message_id: String,
    content_id: String,
    /// The row is held under another mailspace's address.
    foreign_account: Option<String>,
    /// The row is held under a role name this mailspace does not have: it
    /// gets its node when the role is added.
    unowned: bool,
}

impl Mailspace {
    /// Audit backlog citizenship; with `repair`, also fix what is fixable.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure or when a repair
    /// write fails.
    pub fn backlog_audit(&self, repair: bool) -> Result<BacklogAuditReport, VivariumError> {
        let storage = self.storage()?;
        let Some(graph) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? else {
            return Ok(BacklogAuditReport {
                graph: BACKLOG_GRAPH_CODE.to_string(),
                checked: 0,
                findings: Vec::new(),
            });
        };
        let mut nodes = storage.work_graph_nodes(&graph.handle)?;
        let mut edges = storage.work_graph_edges(&graph.handle)?;
        let candidates =
            storage.work_role_messages_updated_since(&audit_since(&graph.created_at))?;
        let mut findings = Vec::new();
        let mut seen_content = HashSet::new();
        let mut checked = 0;
        for message in &candidates {
            if !seen_content.insert(message.content_id.clone()) {
                continue;
            }
            checked += 1;
            audit_candidate(
                self,
                &storage,
                &graph.handle,
                (&mut nodes, &mut edges),
                message,
                repair,
                &mut findings,
            )?;
        }
        audit_orphan_nodes(&storage, &nodes, &mut findings)?;
        Ok(BacklogAuditReport {
            graph: graph.code,
            checked,
            findings,
        })
    }
}

/// Audit one candidate row's item: its folders against its latest event
/// letter, then its node and edges. A repair refreshes the graph snapshot,
/// so the rest of the audit judges what is there now.
fn audit_candidate(
    mailspace: &Mailspace,
    storage: &Storage,
    graph_handle: &str,
    (nodes, edges): (&mut Vec<WorkGraphNodeRow>, &mut Vec<WorkGraphEdgeRow>),
    message: &StoredMessageView,
    repair: bool,
    findings: &mut Vec<BacklogAuditFinding>,
) -> Result<(), VivariumError> {
    let before = findings.len();
    audit_event_state(mailspace, storage, &message.content_id, repair, findings)?;
    if repair && findings.len() > before {
        *nodes = storage.work_graph_nodes(graph_handle)?;
        *edges = storage.work_graph_edges(graph_handle)?;
    }
    let before = findings.len();
    if let Some(item) = canonical_item(storage, mailspace, message)? {
        audit_item(
            mailspace,
            storage,
            graph_handle,
            nodes,
            &item,
            repair,
            findings,
        )?;
        if !item.unowned && item.foreign_account.is_none() {
            audit_edges(mailspace, storage, nodes, edges, &item, repair, findings)?;
        }
    }
    if repair && findings.len() > before {
        *nodes = storage.work_graph_nodes(graph_handle)?;
        *edges = storage.work_graph_edges(graph_handle)?;
    }
    Ok(())
}

/// Report (and with `repair`, re-apply) an item whose folders disagree with
/// the latest event letter held for it: a checkout copied or restored from
/// before the event, or an event filed by a binary that did not apply it.
fn audit_event_state(
    mailspace: &Mailspace,
    storage: &Storage,
    content_id: &str,
    repair: bool,
    findings: &mut Vec<BacklogAuditFinding>,
) -> Result<(), VivariumError> {
    let Some((kind, rows)) = mailspace.event_drift(storage, content_id)? else {
        return audit_released_event(mailspace, storage, content_id, repair, findings);
    };
    let Some(first) = rows.first() else {
        return Ok(());
    };
    let folders: Vec<String> = rows
        .iter()
        .map(|row| format!("{} in '{}'", row.handle, row.local_role))
        .collect();
    let mut finding = BacklogAuditFinding {
        class: "event_state".into(),
        kind: mailspace.source_kind(storage, first).unwrap_or_default(),
        handle: first.handle.clone(),
        subject: first.subject.clone(),
        detail: format!(
            "the latest event letter says {}, but {}",
            kind.header(),
            folders.join(", ")
        ),
        repaired: false,
    };
    if repair {
        mailspace.repair_event_drift(content_id)?;
        finding.repaired = true;
    }
    findings.push(finding);
    Ok(())
}

/// Report (and with `repair`, settle) an item whose folders match its
/// latest event but that holds a letter skipped as dated past the clock when
/// it arrived, which the clock has since passed: its marks, binds, seal or
/// state have not been applied. A pull settles it too; the audit names it.
fn audit_released_event(
    mailspace: &Mailspace,
    storage: &Storage,
    content_id: &str,
    repair: bool,
    findings: &mut Vec<BacklogAuditFinding>,
) -> Result<(), VivariumError> {
    if !event_release_due(storage, content_id)? {
        return Ok(());
    }
    let Some(first) = storage
        .live_messages_for_content(content_id)?
        .into_iter()
        .next()
    else {
        return Ok(());
    };
    let mut finding = BacklogAuditFinding {
        class: "event_state".into(),
        kind: mailspace.source_kind(storage, &first).unwrap_or_default(),
        handle: first.handle.clone(),
        subject: first.subject.clone(),
        detail: "an event letter dated past the clock when it arrived counts now, but its \
                 marks, binds, seal or state are not applied yet"
            .into(),
        repaired: false,
    };
    if repair {
        mailspace.repair_event_drift(content_id)?;
        finding.repaired = true;
    }
    findings.push(finding);
    Ok(())
}

/// Canonical (lowest message id) work copy of a candidate row.
fn canonical_item(
    storage: &crate::storage::Storage,
    mailspace: &Mailspace,
    message: &StoredMessageView,
) -> Result<Option<AuditItem>, VivariumError> {
    let siblings = storage.message_ids_by_content(&message.content_id)?;
    let message_id = siblings
        .first()
        .cloned()
        .unwrap_or_else(|| message.message_id.clone());
    let Some(view) = storage.message_by_id(&message_id)? else {
        return Ok(None);
    };
    let handle = storage.display_handle(&message_id)?;
    let header_kind = mailspace.source_kind(storage, &view).unwrap_or_default();
    if header_kind == "pump" {
        // A pump record is the pump's own, not backlog work: it has no node.
        return Ok(None);
    }
    let kind = if matches!(header_kind.as_str(), "task" | "need" | "want") {
        header_kind
    } else {
        folder_kind(&view.local_role)
            .map(str::to_string)
            .unwrap_or_else(|| "task".into())
    };
    let folder_done = view.local_role == "done";
    let done = if kind == "want" {
        match mailspace.want_state(&handle) {
            Ok(report) => report.state == WantState::Done,
            Err(_) => folder_done,
        }
    } else {
        folder_done
    };
    Ok(Some(AuditItem {
        handle,
        kind,
        folder_done,
        done,
        foreign_account: is_foreign_account(&view.account).then(|| view.account.clone()),
        unowned: mailspace.is_unowned_account(&view.account),
        content_id: view.content_id,
        subject: view.subject,
        message_id,
    }))
}

/// Item kind implied by a work folder role; `done` preserves the minted kind
/// because the folder no longer records it.
fn folder_kind(role: &str) -> Option<&'static str> {
    match role {
        "tasks" => Some("task"),
        "needs" => Some("need"),
        "wants" => Some("want"),
        _ => None,
    }
}

/// Candidate lower bound: one minute of slack before the graph was created,
/// so the very first send — whose messages are ingested milliseconds before
/// the mint that creates the graph — is still audited, while pre-graph
/// history stays out of scope.
fn audit_since(graph_created_at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(graph_created_at)
        .map(|moment| (moment - chrono::Duration::minutes(1)).to_rfc3339())
        .unwrap_or_else(|_| graph_created_at.to_string())
}

/// Compare one item against the graph, repairing when asked.
fn audit_item(
    mailspace: &Mailspace,
    storage: &crate::storage::Storage,
    graph_handle: &str,
    nodes: &[WorkGraphNodeRow],
    item: &AuditItem,
    repair: bool,
    findings: &mut Vec<BacklogAuditFinding>,
) -> Result<(), VivariumError> {
    let node = backlog_node_by_token(storage, nodes, &item.handle)?;
    if let Some(account) = &item.foreign_account {
        return audit_foreign(mailspace, node.as_ref(), item, account, repair, findings);
    }
    if item.unowned {
        return Ok(());
    }
    let Some(node) = node else {
        let mut finding = BacklogAuditFinding {
            class: "missing_node".into(),
            kind: item.kind.clone(),
            handle: item.handle.clone(),
            subject: item.subject.clone(),
            detail: "work item has no backlog node (send predates node minting or mint failed)"
                .into(),
            repaired: false,
        };
        if repair {
            repair_mint(mailspace, item)?;
            finding.repaired = true;
        }
        findings.push(finding);
        return Ok(());
    };
    audit_kind(mailspace, graph_handle, &node, item, repair, findings)?;
    // A `done` folder under an open node is a report awaiting its
    // acceptance, not drift: the acceptor's `done` completes the node.
    let reported = item.folder_done
        && item.kind != "want"
        && matches!(node.state.as_str(), "open" | "active")
        && mailspace.audit_sees_report(storage, &item.content_id)?;
    if !reported {
        audit_state(mailspace, graph_handle, &node, item, repair, findings)?;
    }
    Ok(())
}

/// An item held under another mailspace's address is that mailspace's work:
/// it never needs a node here, and one that exists is settled, not repaired
/// toward the folder. Reports only a node still open or active.
fn audit_foreign(
    mailspace: &Mailspace,
    node: Option<&WorkGraphNodeRow>,
    item: &AuditItem,
    account: &str,
    repair: bool,
    findings: &mut Vec<BacklogAuditFinding>,
) -> Result<(), VivariumError> {
    let Some(node) = node.filter(|node| matches!(node.state.as_str(), "open" | "active")) else {
        return Ok(());
    };
    let mut finding = BacklogAuditFinding {
        class: "foreign_held".into(),
        kind: node.kind.clone(),
        handle: item.handle.clone(),
        subject: item.subject.clone(),
        detail: format!(
            "node is '{}' but the item is held for {account}",
            node.state
        ),
        repaired: false,
    };
    if repair {
        mailspace.backlog_complete_item_via(&item.handle, "via=repair")?;
        finding.repaired = true;
    }
    findings.push(finding);
    Ok(())
}

/// Mint the missing node for `item` and the edges its headers name,
/// completing it when the folder is done.
fn repair_mint(mailspace: &Mailspace, item: &AuditItem) -> Result<(), VivariumError> {
    let letter = mailspace.storage()?.read_message(&item.message_id)?;
    mailspace.backlog_attach_letter(&item.kind, &item.message_id, &item.subject, &letter)?;
    let reported = item.folder_done
        && item.kind != "want"
        && mailspace.audit_sees_report(&*mailspace.storage()?, &item.content_id)?;
    if item.done && !reported {
        mailspace.backlog_complete_item_via(&item.handle, "via=repair")?;
    }
    let arrived = (item.message_id.clone(), item.content_id.clone());
    mailspace.backlog_settle_pending_dependencies(&[arrived])?;
    Ok(())
}

/// Report (and with `repair`, make) the edges an item's `depends_on`
/// front matter names that the graph lacks.
fn audit_edges(
    mailspace: &Mailspace,
    storage: &Storage,
    nodes: &[WorkGraphNodeRow],
    edges: &[WorkGraphEdgeRow],
    item: &AuditItem,
    repair: bool,
    findings: &mut Vec<BacklogAuditFinding>,
) -> Result<(), VivariumError> {
    let missing = mailspace.missing_edges(storage, nodes, edges, &item.content_id)?;
    let mut repaired: Vec<String> = Vec::new();
    for edge in missing {
        let mut finding = BacklogAuditFinding {
            class: "missing_edge".into(),
            kind: item.kind.clone(),
            handle: edge.handle.clone(),
            subject: item.subject.clone(),
            detail: format!(
                "its Depends-On header names {} ({}) but the graph has no edge from it",
                edge.reference, edge.dependency
            ),
            repaired: false,
        };
        if repair {
            if !repaired.contains(&edge.message_id) {
                let letter = storage.read_message(&edge.message_id)?;
                let refs = super::dependencies::depends_on_refs(&letter);
                mailspace.link_dependencies(&edge.message_id, &refs)?;
                repaired.push(edge.message_id.clone());
            }
            finding.repaired = true;
        }
        findings.push(finding);
    }
    Ok(())
}

fn audit_kind(
    mailspace: &Mailspace,
    graph_handle: &str,
    node: &WorkGraphNodeRow,
    item: &AuditItem,
    repair: bool,
    findings: &mut Vec<BacklogAuditFinding>,
) -> Result<(), VivariumError> {
    if item.folder_done || node.kind == item.kind {
        return Ok(());
    }
    let mut finding = BacklogAuditFinding {
        class: "node_kind".into(),
        kind: item.kind.clone(),
        handle: item.handle.clone(),
        subject: item.subject.clone(),
        detail: format!("node kind is '{}'; folder says '{}'", node.kind, item.kind),
        repaired: false,
    };
    if repair {
        mailspace
            .storage()?
            .set_work_graph_node_kind(graph_handle, &node.handle, &item.kind)?;
        finding.repaired = true;
    }
    findings.push(finding);
    Ok(())
}

fn audit_state(
    mailspace: &Mailspace,
    graph_handle: &str,
    node: &WorkGraphNodeRow,
    item: &AuditItem,
    repair: bool,
    findings: &mut Vec<BacklogAuditFinding>,
) -> Result<(), VivariumError> {
    let done = node.state == "done";
    if done == item.done || node.state == "cancelled" || node.state == "superseded" {
        return Ok(());
    }
    let mut finding = BacklogAuditFinding {
        class: "node_state".into(),
        kind: item.kind.clone(),
        handle: item.handle.clone(),
        subject: item.subject.clone(),
        detail: format!("node is '{}' but {}", node.state, item_side(item)),
        repaired: false,
    };
    if repair {
        repair_state(mailspace, graph_handle, node, item)?;
        finding.repaired = true;
    }
    findings.push(finding);
    Ok(())
}

/// What the audit compares a node's state against, in words.
fn item_side(item: &AuditItem) -> String {
    let state = if item.done { "done" } else { "open" };
    if item.kind == "want" {
        format!("the want's computed state is '{state}'")
    } else {
        format!("the item folder is '{state}'")
    }
}

/// Folder is the lifecycle ledger of record: done folders complete nodes;
/// open folders reopen nodes — except needs with bound units, where a done
/// node means the join fired and only the mailbox settle was missed.
fn repair_state(
    mailspace: &Mailspace,
    graph_handle: &str,
    node: &WorkGraphNodeRow,
    item: &AuditItem,
) -> Result<(), VivariumError> {
    if item.done {
        return mailspace
            .backlog_complete_item_via(&item.handle, "via=repair")
            .map(|_| ());
    }
    if node.kind == "need" && node.subgraph.is_none() {
        let units = mailspace.backlog_bound_units(&item.handle)?;
        if !units.is_empty() && units.iter().all(|unit| unit.state == "done") {
            return mailspace.settle_need_mailbox(&item.handle, true, "repair");
        }
    }
    let storage = mailspace.storage()?;
    storage.set_work_graph_node_state(graph_handle, &node.handle, "open", Some("via=repair"))
}

/// Report nodes whose item no longer exists (deleted or trashed work).
/// Only nodes minted from a message (`backlog_attached` events) count —
/// hand-added graph nodes are topology, not citizenship. Resolution uses
/// one handle-map pass (source ids match a message basis prefix), never a
/// per-node token resolution: boards run to tens of thousands of messages.
fn audit_orphan_nodes(
    storage: &crate::storage::Storage,
    nodes: &[WorkGraphNodeRow],
    findings: &mut Vec<BacklogAuditFinding>,
) -> Result<(), VivariumError> {
    let events = storage.list_work_graph_events_after(0)?;
    let minted: HashSet<&str> = events
        .iter()
        .filter(|event| event.event_type == "backlog_attached")
        .filter_map(|event| event.node_handle.as_deref())
        .collect();
    let handle_map = storage.handle_map()?;
    let bases: Vec<&str> = handle_map
        .keys()
        .map(|message_id| {
            message_id
                .strip_prefix("msg_")
                .unwrap_or(message_id.as_str())
        })
        .collect();
    for node in nodes {
        if !minted.contains(node.handle.as_str())
            || node.source_id.is_empty()
            || bases
                .iter()
                .any(|basis| basis.starts_with(node.source_id.as_str()))
        {
            continue;
        }
        findings.push(BacklogAuditFinding {
            class: "orphan_node".into(),
            kind: node.kind.clone(),
            handle: node.source_id.clone(),
            subject: node.label.clone(),
            detail: "no message resolves to this node; the item was deleted or trashed".into(),
            repaired: false,
        });
    }
    Ok(())
}

/// Print an audit report as compact text or JSON.
///
/// # Errors
/// Returns JSON encode errors or large-stdout refusal.
pub fn print_backlog_audit(
    report: &BacklogAuditReport,
    json: bool,
    confirm_large: bool,
) -> Result<(), VivariumError> {
    if json {
        return crate::stdout_budget::print_pretty_json(
            "graph audit",
            report,
            confirm_large,
            Some(&report.graph),
        );
    }
    println!("backlog audit {}", report.graph);
    println!("  checked  {}", report.checked);
    let count = |class: &str| report.findings.iter().filter(|f| f.class == class).count();
    println!(
        "  findings missing={} state={} kind={} orphan={} foreign={} edge={} event={}",
        count("missing_node"),
        count("node_state"),
        count("node_kind"),
        count("orphan_node"),
        count("foreign_held"),
        count("missing_edge"),
        count("event_state"),
    );
    for finding in &report.findings {
        let flag = if finding.repaired { " [repaired]" } else { "" };
        println!(
            "  {} {} {} — {} ({}){}",
            finding.class, finding.kind, finding.handle, finding.subject, finding.detail, flag
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "backlog_audit_test.rs"]
mod tests;
