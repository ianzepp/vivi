//! Dependency edges as letters.
//!
//! An item is sent with its prerequisites in `X-Vivi-Depends-On` headers.
//! After the send, a `dependency-added` or `dependency-removed` property
//! letter on the item (see `property.rs`) adds or drops prerequisites, one
//! `X-Vivi-Event-Dep` header per dependency naming its `Message-ID`.
//!
//! Per dependency the newest counted letter by `(instant, Message-ID)`
//! says whether the edge is there, and it wins over the header the item
//! was sent with: a header edge is removed by a newer `dependency-removed`
//! letter, and added back by a newer `dependency-added` one. The state is a
//! function of the letters held, so every member lands the same edges
//! whatever order the letters arrive in. A sealed item counts only the
//! letters dated before its seal, as marks and binds do.
//!
//! The same fold decides which references an item's `X-Vivi-Depends-On`
//! headers still stand for: [`Mailspace::dependency_verdicts`] is read by
//! every route that builds edges from the letters (attach, pull, pending
//! settle, `graph audit --repair`), so none of them puts back an edge a
//! newer letter removed.

use std::collections::BTreeMap;

use super::property::{PropertyEvent, PropertyKind, earliest_absorb, property_chain};
use super::{EventTally, HeldItem, Mailspace, held_item};
use crate::error::VivariumError;
use crate::mailspace::backlog::backlog_node_by_token;
use crate::mailspace::dependencies::depends_on_refs;
use crate::storage::{BACKLOG_GRAPH_CODE, Storage};

/// The newest counted letter's word per dependency `Message-ID`: `true`
/// when the edge is there, `false` when a newer letter removed it.
pub(in crate::mailspace) type DependencyVerdicts = BTreeMap<String, bool>;

impl Mailspace {
    /// The dependency letters held for the item behind `content_id`, folded
    /// per dependency. Empty when the item has no replies, is not held work,
    /// or no counted letter names a dependency.
    pub(in crate::mailspace) fn dependency_verdicts(
        &self,
        storage: &Storage,
        content_id: &str,
    ) -> Result<DependencyVerdicts, VivariumError> {
        if storage.reply_child_contents(content_id)?.is_empty() {
            return Ok(DependencyVerdicts::new());
        }
        let Some(item) = held_item(storage, content_id)? else {
            return Ok(DependencyVerdicts::new());
        };
        let chain = property_chain(self, storage, &item)?;
        let seal = earliest_absorb(&chain.events).map(|seal| (seal.at, seal.id.clone()));
        let counted: Vec<&PropertyEvent> = chain
            .events
            .iter()
            .filter(|event| {
                seal.as_ref()
                    .is_none_or(|seal| (event.at, &event.id) < (seal.0, &seal.1))
            })
            .collect();
        Ok(fold_dependencies(counted))
    }

    /// Bring the item's prerequisite edges to what its header and its
    /// dependency letters say: every dependency a newer letter added is
    /// linked on each copy of the item, every one a newer letter removed
    /// has its edge dropped. Idempotent.
    pub(super) fn apply_dependency_events(
        &self,
        storage: &Storage,
        item: &HeldItem,
        tally: &mut EventTally,
    ) -> Result<(), VivariumError> {
        let applied = self.settle_dependency_edges(storage, item)?;
        tally.applied += usize::from(applied);
        Ok(())
    }

    /// [`Mailspace::apply_dependency_events`] for the item behind
    /// `content_id`, returning whether an edge was made or dropped. The
    /// writer of a dependency letter calls it so its own graph moves as the
    /// letter says.
    pub(in crate::mailspace) fn settle_dependency_edges_of(
        &self,
        storage: &Storage,
        content_id: &str,
    ) -> Result<bool, VivariumError> {
        let Some(item) = held_item(storage, content_id)? else {
            return Ok(false);
        };
        self.settle_dependency_edges(storage, &item)
    }

    fn settle_dependency_edges(
        &self,
        storage: &Storage,
        item: &HeldItem,
    ) -> Result<bool, VivariumError> {
        let verdicts = self.dependency_verdicts(storage, &item.content_id)?;
        if verdicts.is_empty() {
            return Ok(false);
        }
        let header_refs = depends_on_refs(&item.bytes);
        let mut changed = 0;
        for row in &item.rows {
            changed += self.link_dependencies(&row.message_id, &header_refs)?;
        }
        for (dep, present) in &verdicts {
            if !present {
                changed += unlink_dependency(storage, item, dep)?;
            }
        }
        Ok(changed > 0)
    }
}

/// Fold dependency letters per dependency: the newest by
/// `(instant, Message-ID)` decides.
fn fold_dependencies(events: Vec<&PropertyEvent>) -> DependencyVerdicts {
    let mut newest: BTreeMap<&str, (&PropertyEvent, bool)> = BTreeMap::new();
    for event in events {
        let added = match event.kind {
            PropertyKind::DependencyAdded => true,
            PropertyKind::DependencyRemoved => false,
            _ => continue,
        };
        for dep in &event.deps {
            let newer = newest
                .get(dep.as_str())
                .is_none_or(|(held, _)| (event.at, &event.id) > (held.at, &held.id));
            if newer {
                newest.insert(dep, (event, added));
            }
        }
    }
    newest
        .into_iter()
        .map(|(dep, (_, added))| (dep.to_string(), added))
        .collect()
}

/// Drop the edges from the dependency `rfc_id` names to every copy of
/// `item`. Returns how many edges went; a dependency not held here, or an
/// item with no node, has none.
fn unlink_dependency(
    storage: &Storage,
    item: &HeldItem,
    rfc_id: &str,
) -> Result<usize, VivariumError> {
    let Some(dep) = storage.live_message_by_rfc_id(rfc_id)? else {
        return Ok(0);
    };
    let Some(graph) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? else {
        return Ok(0);
    };
    let nodes = storage.work_graph_nodes(&graph.handle)?;
    let from = copy_nodes(storage, &nodes, &dep.content_id)?;
    let to = copy_nodes(storage, &nodes, &item.content_id)?;
    let pairs: Vec<(String, String)> = storage
        .work_graph_edges(&graph.handle)?
        .into_iter()
        .filter(|edge| from.contains(&edge.from_node) && to.contains(&edge.to_node))
        .map(|edge| (edge.from_node, edge.to_node))
        .collect();
    storage.remove_backlog_edges(&graph.handle, &pairs)
}

/// The node handles of every copy of the item behind `content_id`.
pub(in crate::mailspace) fn copy_nodes(
    storage: &Storage,
    nodes: &[crate::storage::WorkGraphNodeRow],
    content_id: &str,
) -> Result<Vec<String>, VivariumError> {
    let mut handles = Vec::new();
    for row in storage.live_messages_for_content(content_id)? {
        let token = storage.display_handle(&row.message_id)?;
        if let Some(node) = backlog_node_by_token(storage, nodes, &token)?
            && !handles.contains(&node.handle)
        {
            handles.push(node.handle);
        }
    }
    Ok(handles)
}
