//! Bound units of needs, read from one load of the backlog graph.
//!
//! A board prints a run count for every open need, and each count needs the
//! need's bound units. Loading the whole backlog graph once per need made the
//! board quadratic in the number of needs (5.3 s at 2,000 needs). This module
//! loads the graph once, indexes it, and answers every need from the index.

use std::collections::HashMap;

use super::{BacklogBoundUnit, Mailspace, backlog_node_by_token};
use vivi_error::VivariumError;
use vivi_storage::storage::{BACKLOG_GRAPH_CODE, Storage, WorkGraphNodeRow};

impl Mailspace {
    /// Units bound to the need named by `handle`, with their node states.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub fn backlog_bound_units(
        &self,
        handle: &str,
    ) -> Result<Vec<BacklogBoundUnit>, VivariumError> {
        let handles = [handle.to_string()];
        Ok(self
            .backlog_bound_units_many(&handles)?
            .remove(handle)
            .unwrap_or_default())
    }

    /// Units bound to each need in `handles`, keyed by the handle as given,
    /// from one read of the backlog graph. A handle with no backlog node has
    /// no entry. Each answer equals [`Mailspace::backlog_bound_units`] for
    /// that handle.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub fn backlog_bound_units_many(
        &self,
        handles: &[String],
    ) -> Result<HashMap<String, Vec<BacklogBoundUnit>>, VivariumError> {
        let storage = self.storage()?;
        let mut out = HashMap::new();
        let Some(graph) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? else {
            return Ok(out);
        };
        let nodes = storage.work_graph_nodes(&graph.handle)?;
        let index = NodeIndex::new(&nodes);
        for handle in handles {
            if let Some(parent) = index.node_for_token(&storage, &nodes, handle)? {
                out.insert(handle.clone(), index.units_of(&parent.source_id));
            }
        }
        Ok(out)
    }
}

/// The backlog graph's nodes indexed by source id, and the nodes of each
/// subgraph in source-id order (the order `work_graph_nodes` returns).
struct NodeIndex<'a> {
    by_source: HashMap<&'a str, &'a WorkGraphNodeRow>,
    by_subgraph: HashMap<&'a str, Vec<&'a WorkGraphNodeRow>>,
}

impl<'a> NodeIndex<'a> {
    fn new(nodes: &'a [WorkGraphNodeRow]) -> Self {
        let mut by_source = HashMap::new();
        let mut by_subgraph: HashMap<&str, Vec<&WorkGraphNodeRow>> = HashMap::new();
        for node in nodes {
            by_source.entry(node.source_id.as_str()).or_insert(node);
            if let Some(parent) = node.subgraph.as_deref() {
                by_subgraph.entry(parent).or_default().push(node);
            }
        }
        Self {
            by_source,
            by_subgraph,
        }
    }

    /// The node `token` names: its source id when it is one, else the
    /// general lookup (message tokens, ids and prefixes).
    fn node_for_token(
        &self,
        storage: &Storage,
        nodes: &[WorkGraphNodeRow],
        token: &str,
    ) -> Result<Option<WorkGraphNodeRow>, VivariumError> {
        if let Some(node) = self.by_source.get(token) {
            return Ok(Some((*node).clone()));
        }
        backlog_node_by_token(storage, nodes, token)
    }

    fn units_of(&self, parent_source_id: &str) -> Vec<BacklogBoundUnit> {
        self.by_subgraph
            .get(parent_source_id)
            .map(|units| {
                units
                    .iter()
                    .map(|unit| BacklogBoundUnit {
                        handle: unit.source_id.clone(),
                        state: unit.state.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}
