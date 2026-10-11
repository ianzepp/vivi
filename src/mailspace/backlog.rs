//! Backlog citizenship: needs and wants as work-graph nodes in the
//! per-mailspace `backlog` graph, so readiness spans the whole backlog
//! without a separate graph-management step.

use serde::Serialize;

use super::Mailspace;
use super::graph_mutate::{newly_ready_after_done, ready_handles, validate_source_id};
use crate::error::VivariumError;
use crate::storage::{
    BACKLOG_GRAPH_CODE, BacklogMintInput, BacklogNodeInput, MailspaceEventInput, Storage,
    WorkGraphEdgeInput, WorkGraphNodeRow,
};

/// A resolved `--depends-on` reference to an existing need or want.
struct BacklogDep {
    handle: String,
    subject: String,
    state: &'static str,
    kind: &'static str,
}

impl Mailspace {
    /// Validate `--depends-on` handles before a send so a typo fails before
    /// the message exists.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] naming the first dependency that does not
    /// resolve to an open or done need/want.
    pub fn backlog_validate_deps(&self, depends_on: &[String]) -> Result<(), VivariumError> {
        let storage = self.storage()?;
        for dep in depends_on {
            resolve_backlog_dep(&storage, dep)?;
        }
        Ok(())
    }

    /// Mint one backlog node for `handle` (label = subject) plus one edge per
    /// dependency, minting missing dependency nodes retroactively. Idempotent.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on invalid handles, unresolvable
    /// dependencies, or storage failure.
    pub fn backlog_attach(
        &self,
        kind: &str,
        handle: &str,
        subject: &str,
        depends_on: &[String],
    ) -> Result<(), VivariumError> {
        validate_source_id(handle)?;
        let mut storage = self.storage()?;
        let mut nodes = vec![BacklogNodeInput {
            source_id: handle.to_string(),
            label: subject.to_string(),
            state: "open".into(),
            kind: kind.to_string(),
        }];
        let mut edges = Vec::new();
        for dep in depends_on {
            let resolved = resolve_backlog_dep(&storage, dep)?;
            edges.push(WorkGraphEdgeInput {
                from_source_id: resolved.handle.clone(),
                to_source_id: handle.to_string(),
                label: None,
                style: "solid".into(),
            });
            nodes.push(BacklogNodeInput {
                source_id: resolved.handle,
                label: resolved.subject,
                state: resolved.state.into(),
                kind: resolved.kind.into(),
            });
        }
        storage.mint_backlog(&BacklogMintInput { nodes, edges })?;
        Ok(())
    }

    /// Complete the backlog node for `handle` via the lifecycle hook, then
    /// cascade the join rule. Returns whether a transition occurred.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub fn backlog_complete_item(&self, handle: &str) -> Result<bool, VivariumError> {
        self.backlog_complete_item_via(handle, "via=lifecycle")
    }

    /// Complete the backlog node for `handle`, recording the decision
    /// (`via=<origin>`) atomically with the transition, then cascade the join
    /// rule. Returns whether a transition occurred; no-op when the backlog
    /// graph or the node does not exist (pre-feature items) or the node is
    /// not completable.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub fn backlog_complete_item_via(
        &self,
        handle: &str,
        via: &str,
    ) -> Result<bool, VivariumError> {
        let mut storage = self.storage()?;
        let Some(graph) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? else {
            return Ok(false);
        };
        let nodes = storage.work_graph_nodes(&graph.handle)?;
        let edges = storage.work_graph_edges(&graph.handle)?;
        let Some(target) = backlog_node_by_token(&storage, &nodes, handle)? else {
            return Ok(false);
        };
        if !matches!(target.state.as_str(), "open" | "active") {
            return Ok(false);
        }
        let ready_before = ready_handles(&nodes, &edges);
        let newly_ready = newly_ready_after_done(&nodes, &edges, &target.handle, &ready_before);
        storage.complete_work_graph_node(
            &graph.handle,
            &target.handle,
            None,
            &newly_ready,
            Some(via),
        )?;
        let siblings = sibling_display_handles(&storage, handle)?;
        drop(storage);
        // A multi-recipient item is one work item with per-copy nodes:
        // completing any copy completes the copies' nodes too.
        for sibling in siblings {
            self.backlog_complete_item_via(&sibling, "via=sibling")?;
        }
        if let Some(parent) = target.subgraph.clone() {
            self.backlog_complete_if_join_complete(&parent)?;
        }
        Ok(true)
    }

    /// Bind unit tasks to a parent need: each unit node's `subgraph` becomes
    /// the need's source id. Binding already-done units may complete the need
    /// immediately via the join rule.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] naming the first missing node, or on
    /// storage failure.
    pub fn backlog_bind_units(&self, parent: &str, units: &[String]) -> Result<(), VivariumError> {
        validate_source_id(parent)?;
        if units.is_empty() {
            return Err(VivariumError::Message(
                "bind needs at least one unit handle".into(),
            ));
        }
        let mut storage = self.storage()?;
        let graph = storage
            .work_graph_by_code(BACKLOG_GRAPH_CODE)?
            .ok_or_else(|| VivariumError::Message(format!("need not found: {parent}")))?;
        let nodes = storage.work_graph_nodes(&graph.handle)?;
        let parent_node = backlog_node_by_token(&storage, &nodes, parent)?
            .ok_or_else(|| VivariumError::Message(format!("need not found: {parent}")))?;
        if parent_node.kind != "need" {
            return Err(VivariumError::Message(format!(
                "bind lowers unit tasks onto needs; '{}' is a {}",
                parent, parent_node.kind
            )));
        }
        if parent_node.state != "open" {
            return Err(VivariumError::Message(format!(
                "cannot bind units to need '{parent}' in state '{}'",
                parent_node.state
            )));
        }
        let mut unit_handles = Vec::with_capacity(units.len());
        for unit in units {
            let unit_node = backlog_node_by_token(&storage, &nodes, unit)?.ok_or_else(|| {
                VivariumError::Message(format!(
                    "unit '{unit}' has no graph node; send it before binding (vivi graph \
                         audit --repair recovers untracked items)"
                ))
            })?;
            unit_handles.push(unit_node.handle.clone());
        }
        storage.bind_backlog_units(&graph.handle, parent, &unit_handles)?;
        drop(storage);
        self.backlog_complete_if_join_complete(parent)
    }

    /// Complete `parent` when every unit bound to it is done, then settle the
    /// need's mailbox copies so folder state matches the node transition —
    /// the join automates the need's completion, not only its graph node.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    fn backlog_complete_if_join_complete(&self, parent: &str) -> Result<(), VivariumError> {
        let storage = self.storage()?;
        let Some(graph) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? else {
            return Ok(());
        };
        let units = storage.work_graph_nodes_by_subgraph(&graph.handle, parent)?;
        if units.is_empty() || !units.iter().all(|n| n.state == "done") {
            return Ok(());
        }
        drop(storage);
        if self.backlog_complete_item(parent)? {
            self.settle_need_mailbox(parent, true, "join")?;
        }
        Ok(())
    }

    /// Move every active copy of the need named by `handle` between `needs`
    /// and `done`, logging `need done` / `need reopen` events tagged
    /// `via=<via>`. Sealed (absorbed) copies stay untouched. Used by the join
    /// rule and its reopen reversal so mailbox folders track graph joins.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub(super) fn settle_need_mailbox(
        &self,
        handle: &str,
        to_done: bool,
        via: &str,
    ) -> Result<(), VivariumError> {
        let mut storage = self.storage()?;
        let message_id = match storage.resolve_message_token(handle) {
            Ok(message_id) => message_id,
            Err(_) => return Ok(()),
        };
        let Some(view) = storage.message_by_id(&message_id)? else {
            return Ok(());
        };
        let (from_role, to_role, command) = if to_done {
            ("needs", "done", "need done")
        } else {
            ("done", "needs", "need reopen")
        };
        for sibling_id in storage.message_ids_by_content(&view.content_id)? {
            let Some(copy) = storage.message_by_id(&sibling_id)? else {
                continue;
            };
            if copy.local_role != from_role || copy.absorbed_at.is_some() {
                continue;
            }
            storage.move_message_to_role(&copy.account, &sibling_id, to_role)?;
            storage.append_mailspace_event(&MailspaceEventInput {
                command: command.into(),
                event_type: "moved".into(),
                actor_identity: None,
                account: copy.account.clone(),
                message_id: sibling_id.clone(),
                content_id: copy.content_id.clone(),
                from_role: Some(copy.local_role.clone()),
                to_role: Some(to_role.into()),
                from_identity: None,
                to_identity: Some(copy.account.clone()),
                subject: copy.subject.clone(),
                note: Some(format!("via={via}")),
            })?;
        }
        Ok(())
    }

    /// Reopen the backlog node for `handle` after a done→open lifecycle move,
    /// cascading to the subgraph parent: reopening a bound unit invalidates
    /// the join, so a done parent need re-opens too (and its mailbox copies
    /// return to `needs`). No-op when the graph, the node, or a non-done
    /// state says so.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub fn backlog_reopen_item(&self, handle: &str) -> Result<(), VivariumError> {
        self.reopen_item_cascading(handle, &mut Vec::new())
            .map(|_| ())
    }

    /// Returns the kind of the reopened node, or `None` when no transition
    /// happened.
    fn reopen_item_cascading(
        &self,
        handle: &str,
        visited: &mut Vec<String>,
    ) -> Result<Option<String>, VivariumError> {
        if visited.iter().any(|seen| seen == handle) {
            return Ok(None);
        }
        visited.push(handle.to_string());
        let mut storage = self.storage()?;
        let Some(graph) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? else {
            return Ok(None);
        };
        let nodes = storage.work_graph_nodes(&graph.handle)?;
        let Some(target) = backlog_node_by_token(&storage, &nodes, handle)? else {
            return Ok(None);
        };
        if target.state != "done" {
            return Ok(None);
        }
        let parent = target.subgraph.clone();
        storage.set_work_graph_node_state(&graph.handle, &target.handle, "open", None)?;
        let siblings = sibling_display_handles(&storage, handle)?;
        drop(storage);
        for sibling in siblings {
            self.reopen_item_cascading(&sibling, visited)?;
        }
        if let Some(parent) = parent {
            let reopened_kind = self.reopen_item_cascading(&parent, visited)?;
            if reopened_kind.as_deref() == Some("need") {
                self.settle_need_mailbox(&parent, false, "join-reopen")?;
            }
        }
        Ok(Some(target.kind))
    }

    /// Keep the backlog node in step with a lifecycle folder move. Work-item
    /// closes (tasks/needs/wants → done, and the sender's own `sent` copy →
    /// done) and their reversals touch graph state; promotion (wants→needs)
    /// intentionally leaves the node open.
    pub(super) fn sync_backlog_node(
        &self,
        from_role: &str,
        to_role: &str,
        handle: &str,
    ) -> Result<(), VivariumError> {
        match (from_role, to_role) {
            ("tasks" | "needs" | "wants" | "sent", "done") => {
                self.backlog_complete_item(handle).map(|_| ())
            }
            ("done", "tasks" | "needs" | "wants") => self.backlog_reopen_item(handle),
            _ => Ok(()),
        }
    }

    /// Add a post-hoc prerequisite edge between two existing backlog items:
    /// `dependent` stops being ready until `prereq` completes. Idempotent;
    /// refuses self edges, frozen dependents, and unknown items.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] naming the offending item.
    pub fn backlog_connect(
        &self,
        dependent: &str,
        prereq: &str,
        label: Option<&str>,
    ) -> Result<(), VivariumError> {
        let mut storage = self.storage()?;
        let graph = storage
            .work_graph_by_code(BACKLOG_GRAPH_CODE)?
            .ok_or_else(|| VivariumError::Message("no backlog graph; send work first".into()))?;
        let nodes = storage.work_graph_nodes(&graph.handle)?;
        let dependent_node =
            backlog_node_by_token(&storage, &nodes, dependent)?.ok_or_else(|| {
                VivariumError::Message(format!(
                    "dependent '{dependent}' has no graph node (vivi graph audit --repair recovers \
                 untracked items)"
                ))
            })?;
        let prereq_node = backlog_node_by_token(&storage, &nodes, prereq)?.ok_or_else(|| {
            VivariumError::Message(format!(
                "prerequisite '{prereq}' has no graph node (vivi graph audit --repair recovers \
                 untracked items)"
            ))
        })?;
        if dependent_node.handle == prereq_node.handle {
            return Err(VivariumError::Message(format!(
                "an item cannot depend on itself: {dependent}"
            )));
        }
        if matches!(dependent_node.state.as_str(), "active" | "done") {
            return Err(VivariumError::Message(format!(
                "cannot add prerequisite to {} item '{dependent}'",
                dependent_node.state
            )));
        }
        storage.mint_backlog(&BacklogMintInput {
            nodes: Vec::new(),
            edges: vec![WorkGraphEdgeInput {
                from_source_id: prereq_node.source_id,
                to_source_id: dependent_node.source_id,
                label: label.map(str::to_string),
                style: "solid".into(),
            }],
        })?;
        Ok(())
    }

    /// Units bound to the need named by `handle`, with their node states.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub fn backlog_bound_units(
        &self,
        handle: &str,
    ) -> Result<Vec<BacklogBoundUnit>, VivariumError> {
        let storage = self.storage()?;
        let Some(graph) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? else {
            return Ok(Vec::new());
        };
        let nodes = storage.work_graph_nodes(&graph.handle)?;
        let Some(parent) = backlog_node_by_token(&storage, &nodes, handle)? else {
            return Ok(Vec::new());
        };
        Ok(storage
            .work_graph_nodes_by_subgraph(&graph.handle, &parent.source_id)?
            .into_iter()
            .map(|unit| BacklogBoundUnit {
                handle: unit.source_id,
                state: unit.state,
            })
            .collect())
    }

    /// Whether the work item named by `handle` has a backlog node at all —
    /// distinguishes "already settled via lifecycle" from "never tracked".
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub fn backlog_item_tracked(&self, handle: &str) -> Result<bool, VivariumError> {
        let storage = self.storage()?;
        let Some(graph) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? else {
            return Ok(false);
        };
        let nodes = storage.work_graph_nodes(&graph.handle)?;
        Ok(backlog_node_by_token(&storage, &nodes, handle)?.is_some())
    }
}

/// One bound unit of a need, as shown by `need show`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BacklogBoundUnit {
    pub handle: String,
    pub state: String,
}

/// Find the backlog node for a mail token. Falls back to matching the
/// token's message against node source ids as basis prefixes, so a handle
/// that drifted (shortest-prefix growth after later sends) still finds the
/// node minted for the same message.
pub(super) fn backlog_node_by_token(
    storage: &Storage,
    nodes: &[WorkGraphNodeRow],
    token: &str,
) -> Result<Option<WorkGraphNodeRow>, VivariumError> {
    if let Some(found) = nodes.iter().find(|n| n.source_id == token) {
        return Ok(Some(found.clone()));
    }
    let Ok(message_id) = storage.resolve_message_token(token) else {
        return Ok(None);
    };
    let view = storage.message_by_id(&message_id)?;
    let Some(view) = view else {
        return Ok(None);
    };
    let basis = view
        .message_id
        .strip_prefix("msg_")
        .unwrap_or(&view.message_id);
    Ok(nodes
        .iter()
        .find(|n| basis.starts_with(n.source_id.as_str()))
        .cloned())
}

/// Resolve one dependency token to a canonical backlog dep. Accepts open
/// tasks, needs, and wants, the sender's own copy of work sent to another
/// mailspace (open, as it is tracked here), plus done items of any kind.
fn resolve_backlog_dep(storage: &Storage, token: &str) -> Result<BacklogDep, VivariumError> {
    let message_id = storage
        .resolve_message_token(token)
        .map_err(|e| VivariumError::Message(format!("dependency '{token}': {e}")))?;
    let view = storage
        .message_by_id(&message_id)?
        .ok_or_else(|| VivariumError::Message(format!("dependency not found: {token}")))?;
    let (kind, state) = match view.local_role.as_str() {
        "tasks" => ("task", "open"),
        "needs" => ("need", "open"),
        "wants" => ("want", "open"),
        "done" => ("item", "done"),
        "sent" => (sent_work_kind(storage, &message_id, token)?, "open"),
        other => {
            return Err(VivariumError::Message(format!(
                "dependency '{token}' has role '{other}'; expected a task, need, or want handle"
            )));
        }
    };
    let handle = canonical_content_handle(storage, &message_id)?;
    validate_source_id(&handle)
        .map_err(|e| VivariumError::Message(format!("dependency '{token}': {e}")))?;
    Ok(BacklogDep {
        handle,
        subject: view.subject,
        state,
        kind,
    })
}

/// The work kind of a sent copy, from the letter's own kind header. Sent
/// mail that is not work is not a dependency.
fn sent_work_kind(
    storage: &Storage,
    message_id: &str,
    token: &str,
) -> Result<&'static str, VivariumError> {
    let data = storage.read_message(message_id)?;
    match super::kind::message_kind(&data).as_deref() {
        Some("task") => Ok("task"),
        Some("need") => Ok("need"),
        Some("want") => Ok("want"),
        _ => Err(VivariumError::Message(format!(
            "dependency '{token}' has role 'sent'; expected a task, need, or want handle"
        ))),
    }
}

/// Canonical display handle for a message's content: a multi-recipient send
/// delivers per-identity copies of one item, so dependency edges cite one
/// deterministic copy (lowest message id) rather than whichever copy the
/// sender happened to reference.
fn canonical_content_handle(storage: &Storage, message_id: &str) -> Result<String, VivariumError> {
    let Some(view) = storage.message_by_id(message_id)? else {
        return Err(VivariumError::Message(format!(
            "message not found: {message_id}"
        )));
    };
    let canonical = storage
        .message_ids_by_content(&view.content_id)?
        .into_iter()
        .min()
        .unwrap_or_else(|| message_id.to_string());
    storage.display_handle(&canonical)
}

/// Display handles of the other active copies of this handle's content.
fn sibling_display_handles(storage: &Storage, handle: &str) -> Result<Vec<String>, VivariumError> {
    let message_id = storage.resolve_message_token(handle)?;
    content_sibling_handles(storage, &message_id)
}

/// Display handles of the other active copies of this message's content.
fn content_sibling_handles(
    storage: &Storage,
    message_id: &str,
) -> Result<Vec<String>, VivariumError> {
    let Some(view) = storage.message_by_id(message_id)? else {
        return Ok(Vec::new());
    };
    storage
        .message_ids_by_content(&view.content_id)?
        .into_iter()
        .filter(|candidate| candidate != message_id)
        .map(|candidate| storage.display_handle(&candidate))
        .collect()
}

#[cfg(test)]
#[path = "backlog_test.rs"]
mod tests;
