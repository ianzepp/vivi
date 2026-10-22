//! Dependency edges on an item that was already sent.
//!
//! An item is sent with its prerequisites in `X-Vivi-Depends-On` headers.
//! [`Mailspace::set_dependencies`] adds or drops prerequisites afterwards
//! and writes each change as a `dependency-added` / `dependency-removed`
//! property letter on the item, so the change travels like a mark and the
//! newest letter per dependency controls on every member (see
//! `item_events/dependency.rs` for the fold). Only a role that may close
//! the item writes one: its author, a mind, or an operator. Any other role
//! is refused, because an edge only it holds would never travel.

use std::collections::BTreeSet;

use serde::Serialize;

use super::Mailspace;
use super::backlog::{resolve_backlog_dep, resolve_dep_message};
use super::dependencies::{dependency_rfc_id, depends_on_refs, reaches};
use super::item_events::{HeldItem, PropertyKind, copy_nodes, held_item};
use crate::error::VivariumError;
use crate::storage::{BACKLOG_GRAPH_CODE, Storage, StoredMessageView};

/// The header a dependency letter names each dependency's `Message-ID` in.
const EVENT_DEP_HEADER: &str = "X-Vivi-Event-Dep";

/// One dependency after a `depend` command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DependWrite {
    /// Display handle of the dependency.
    pub handle: String,
    /// Whether the command added the dependency (else removed it).
    pub added: bool,
    /// Whether this command wrote a change for it.
    pub changed: bool,
}

/// A dependency of an item as `need show` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BacklogDependency {
    /// Handle of the dependency, or the reference still waiting.
    pub handle: String,
    /// `open`, `active`, `done`, or `awaiting` for a reference this
    /// mailspace does not hold yet.
    pub state: String,
}

/// A dependency a command names, resolved.
struct Target {
    rfc_id: String,
    content_id: String,
    handle: String,
}

impl Mailspace {
    /// Add and remove prerequisite edges on the item `handle` names, as the
    /// role `identity`. A dependency already in the requested state
    /// changes nothing. Every handle resolves before anything is written,
    /// and a failure writes no letter.
    ///
    /// # Errors
    /// Returns an error when no change is asked for, a handle does not
    /// resolve, a dependency is asked both added and removed, the item is
    /// the wrong kind, absorbed, or done, `identity` may not close it, or an
    /// added dependency is the item itself or would close a cycle.
    pub fn set_dependencies(
        &self,
        identity: &str,
        handle: &str,
        kind: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<Vec<DependWrite>, VivariumError> {
        if add.is_empty() && remove.is_empty() {
            return Err(VivariumError::Message(
                "name at least one dependency to --add or --remove".into(),
            ));
        }
        let (identity, message) = self.resolve_owned_message(identity, handle)?;
        let storage = self.storage()?;
        let item = self.writable_item(&storage, &identity, handle, kind, &message)?;
        let to_add = resolve_targets(&storage, add, true)?;
        let to_remove = resolve_targets(&storage, remove, false)?;
        refuse_contradictions(&item, &to_add, &to_remove)?;
        Self::check_edit_allowed(&storage, &message.message_id, &item.content_id, &to_add)?;
        let (declared, linked) =
            self.current_dependencies(&storage, &message.message_id, &item.content_id)?;
        let plan = plan_changes(to_add, to_remove, &declared, &linked);
        for (letter, ids) in [
            (PropertyKind::DependencyRemoved, &plan.removed),
            (PropertyKind::DependencyAdded, &plan.added),
        ] {
            if ids.is_empty() {
                continue;
            }
            let extras: Vec<(&str, String)> = ids
                .iter()
                .map(|id| (EVENT_DEP_HEADER, format!("<{id}>")))
                .collect();
            self.emit_property_event(&storage, &item, &identity, letter, &extras, None)?;
        }
        if !plan.added.is_empty() || !plan.removed.is_empty() {
            self.settle_dependency_edges_of(&storage, &item.content_id)?;
        }
        Ok(plan.writes)
    }

    /// The held work item `message` belongs to, once `identity` is known to
    /// be allowed to change its dependencies: it is a `kind`, not absorbed,
    /// and `identity` may close it for everyone.
    fn writable_item(
        &self,
        storage: &Storage,
        identity: &str,
        handle: &str,
        kind: &str,
        message: &StoredMessageView,
    ) -> Result<HeldItem, VivariumError> {
        if self.source_kind(storage, message)? != kind {
            return Err(VivariumError::Message(format!("{handle} is not a {kind}")));
        }
        if message.absorbed_at.is_some() {
            return Err(VivariumError::Message(format!(
                "{handle} is absorbed and can no longer be changed"
            )));
        }
        let Some(item) = held_item(storage, &message.content_id)? else {
            return Err(VivariumError::Message(format!("{handle} is not held work")));
        };
        if !self.may_close(&item, identity) {
            return Err(VivariumError::Message(format!(
                "{identity} may not change the dependencies of {handle}: only its author, a \
                 mind, or an operator may"
            )));
        }
        Ok(item)
    }

    /// Refuse a change the graph cannot take: a done item, an active item
    /// gaining a prerequisite, or an added dependency that already depends
    /// on the item.
    fn check_edit_allowed(
        storage: &Storage,
        message_id: &str,
        content_id: &str,
        to_add: &[Target],
    ) -> Result<(), VivariumError> {
        let Some(graph) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? else {
            return Ok(());
        };
        let nodes = storage.work_graph_nodes(&graph.handle)?;
        let handle = storage.display_handle(message_id)?;
        let own_nodes = copy_nodes(storage, &nodes, content_id)?;
        let states: Vec<&str> = nodes
            .iter()
            .filter(|node| own_nodes.contains(&node.handle))
            .map(|node| node.state.as_str())
            .collect();
        if states.contains(&"done") {
            return Err(VivariumError::Message(format!(
                "cannot change the dependencies of done item '{handle}'"
            )));
        }
        if states.contains(&"active") && !to_add.is_empty() {
            return Err(VivariumError::Message(format!(
                "cannot add prerequisite to active item '{handle}'"
            )));
        }
        let edges = storage.work_graph_edges(&graph.handle)?;
        for target in to_add {
            for dep_node in copy_nodes(storage, &nodes, &target.content_id)? {
                if own_nodes.iter().any(|own| reaches(&edges, own, &dep_node)) {
                    return Err(VivariumError::Message(format!(
                        "{} already depends on {handle}; the edge would close a cycle",
                        target.handle
                    )));
                }
            }
        }
        Ok(())
    }

    /// The dependencies the item stands for now by `Message-ID`: those its
    /// header and its dependency letters name, and, apart, those only the
    /// local graph has an edge from (`graph connect`).
    fn current_dependencies(
        &self,
        storage: &Storage,
        message_id: &str,
        content_id: &str,
    ) -> Result<(BTreeSet<String>, BTreeSet<String>), VivariumError> {
        let header = depends_on_refs(&storage.read_message(message_id)?);
        let declared: BTreeSet<String> = self
            .effective_dependency_refs(storage, message_id, &header)?
            .iter()
            .filter_map(|token| dependency_rfc_id(storage, token))
            .collect();
        let mut linked = BTreeSet::new();
        if let Some(graph) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? {
            let nodes = storage.work_graph_nodes(&graph.handle)?;
            let own = copy_nodes(storage, &nodes, content_id)?;
            for edge in storage.work_graph_edges(&graph.handle)? {
                if !own.contains(&edge.to_node) {
                    continue;
                }
                let source = nodes
                    .iter()
                    .find(|node| node.handle == edge.from_node)
                    .map(|node| node.source_id.as_str());
                if let Some(rfc) = source.and_then(|token| dependency_rfc_id(storage, token)) {
                    linked.insert(rfc);
                }
            }
        }
        Ok((declared, linked))
    }

    /// The prerequisites of the item `handle` names, with their states: the
    /// graph's edges into it, and the references it still waits on.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub fn backlog_dependencies(
        &self,
        handle: &str,
    ) -> Result<Vec<BacklogDependency>, VivariumError> {
        let storage = self.storage()?;
        let Some(graph) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? else {
            return Ok(Vec::new());
        };
        let Ok(message_id) = storage.resolve_message_token(handle) else {
            return Ok(Vec::new());
        };
        let Some(view) = storage.message_by_id(&message_id)? else {
            return Ok(Vec::new());
        };
        let nodes = storage.work_graph_nodes(&graph.handle)?;
        let own = copy_nodes(&storage, &nodes, &view.content_id)?;
        let mut found: Vec<BacklogDependency> = Vec::new();
        for edge in storage.work_graph_edges(&graph.handle)? {
            if !own.contains(&edge.to_node) {
                continue;
            }
            if let Some(node) = nodes.iter().find(|node| node.handle == edge.from_node)
                && !found.iter().any(|dep| dep.handle == node.source_id)
            {
                found.push(BacklogDependency {
                    handle: node.source_id.clone(),
                    state: node.state.clone(),
                });
            }
        }
        let copies = storage.message_ids_by_content(&view.content_id)?;
        for (message_id, refs) in storage.pending_dependencies()? {
            if !copies.contains(&message_id) {
                continue;
            }
            for token in refs {
                if !found.iter().any(|dep| dep.handle == token) {
                    found.push(BacklogDependency {
                        handle: token,
                        state: "awaiting".into(),
                    });
                }
            }
        }
        found.sort_by(|a, b| a.handle.cmp(&b.handle));
        Ok(found)
    }
}

/// Refuse a dependency asked both added and removed, and an added one that
/// is the item itself.
fn refuse_contradictions(
    item: &HeldItem,
    to_add: &[Target],
    to_remove: &[Target],
) -> Result<(), VivariumError> {
    if let Some(both) = to_add
        .iter()
        .find(|a| to_remove.iter().any(|r| r.rfc_id == a.rfc_id))
    {
        return Err(VivariumError::Message(format!(
            "{} is named in both --add and --remove",
            both.handle
        )));
    }
    if let Some(own) = to_add
        .iter()
        .find(|a| a.rfc_id == item.rfc_id || a.content_id == item.content_id)
    {
        return Err(VivariumError::Message(format!(
            "an item cannot depend on itself: {}",
            own.handle
        )));
    }
    Ok(())
}

/// What a command changes: the per-dependency report and the `Message-ID`s
/// each letter names.
struct Plan {
    writes: Vec<DependWrite>,
    added: Vec<String>,
    removed: Vec<String>,
}

/// Decide which of the asked dependencies change anything. An add changes
/// what the header and letters stand for; a remove changes it when they, or
/// a local edge, hold the dependency.
fn plan_changes(
    to_add: Vec<Target>,
    to_remove: Vec<Target>,
    declared: &BTreeSet<String>,
    linked: &BTreeSet<String>,
) -> Plan {
    let mut plan = Plan {
        writes: Vec::new(),
        added: Vec::new(),
        removed: Vec::new(),
    };
    for target in to_add {
        let changed = !declared.contains(&target.rfc_id);
        plan.writes.push(DependWrite {
            handle: target.handle,
            added: true,
            changed,
        });
        if changed {
            plan.added.push(target.rfc_id);
        }
    }
    for target in to_remove {
        let changed = declared.contains(&target.rfc_id) || linked.contains(&target.rfc_id);
        plan.writes.push(DependWrite {
            handle: target.handle,
            added: false,
            changed,
        });
        if changed {
            plan.removed.push(target.rfc_id);
        }
    }
    plan
}

/// Resolve every handle before anything is written. With `held_work`, the
/// dependency must be work this mailspace tracks (the rule `--depends-on`
/// applies at send); a removal needs only a letter it holds.
fn resolve_targets(
    storage: &Storage,
    tokens: &[String],
    held_work: bool,
) -> Result<Vec<Target>, VivariumError> {
    let mut targets: Vec<Target> = Vec::new();
    for token in tokens {
        if held_work {
            resolve_backlog_dep(storage, token)?;
        }
        let message_id = resolve_dep_message(storage, token)
            .map_err(|e| VivariumError::Message(format!("dependency '{token}': {e}")))?;
        let view = storage
            .message_by_id(&message_id)?
            .ok_or_else(|| VivariumError::Message(format!("dependency not found: {token}")))?;
        let rfc_id = view.normalized_message_id.clone().ok_or_else(|| {
            VivariumError::Message(format!("dependency '{token}' has no Message-ID"))
        })?;
        if !targets.iter().any(|seen| seen.rfc_id == rfc_id) {
            targets.push(Target {
                rfc_id,
                content_id: view.content_id.clone(),
                handle: storage.display_handle(&view.message_id)?,
            });
        }
    }
    Ok(targets)
}

#[cfg(test)]
#[path = "depend_test.rs"]
mod tests;
