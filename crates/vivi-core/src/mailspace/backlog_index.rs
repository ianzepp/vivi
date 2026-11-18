//! The backlog graph's nodes, loaded once and indexed.
//!
//! [`super::backlog::backlog_node_by_token`] answers one token from a node
//! slice with a linear scan. A caller that asks about many items (one want
//! state per want, one closed check per serving need) used to reload the
//! slice for every question, so its cost grew with the square of the
//! mailspace. This index is built from one load and answers every token
//! without another scan, with the same answers.

use std::collections::HashMap;

use vivi_error::VivariumError;
use vivi_storage::storage::{BACKLOG_GRAPH_CODE, Storage, WorkGraphNodeRow};

/// The nodes of the backlog graph, keyed by source id.
pub(super) struct BacklogIndex {
    by_source: HashMap<String, WorkGraphNodeRow>,
    /// Distinct source-id lengths, ascending: the only prefix lengths of a
    /// message basis that can name a node.
    lengths: Vec<usize>,
    /// Source ids of each subgraph's nodes (a need's bound units), in
    /// source-id order.
    by_subgraph: HashMap<String, Vec<String>>,
}

impl BacklogIndex {
    /// Load the backlog graph's nodes once. A mailspace with no backlog graph
    /// yields an empty index.
    pub(super) fn load(storage: &Storage) -> Result<Self, VivariumError> {
        let nodes = match storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? {
            Some(graph) => storage.work_graph_nodes(&graph.handle)?,
            None => Vec::new(),
        };
        Ok(Self::new(nodes))
    }

    fn new(nodes: Vec<WorkGraphNodeRow>) -> Self {
        let mut lengths: Vec<usize> = nodes.iter().map(|node| node.source_id.len()).collect();
        lengths.sort_unstable();
        lengths.dedup();
        let mut by_subgraph: HashMap<String, Vec<String>> = HashMap::new();
        for node in &nodes {
            if let Some(parent) = &node.subgraph {
                by_subgraph
                    .entry(parent.clone())
                    .or_default()
                    .push(node.source_id.clone());
            }
        }
        for units in by_subgraph.values_mut() {
            units.sort();
        }
        let by_source = nodes
            .into_iter()
            .map(|node| (node.source_id.clone(), node))
            .collect();
        Self {
            by_source,
            lengths,
            by_subgraph,
        }
    }

    /// The node `token` names: a source id, else the node minted for the
    /// message the token resolves to (its source id is a prefix of the
    /// message id), so a handle that drifted still finds its node. The
    /// shortest such prefix wins, which is the node `backlog_node_by_token`
    /// finds first in source-id order.
    pub(super) fn node_by_token(
        &self,
        storage: &Storage,
        token: &str,
    ) -> Result<Option<&WorkGraphNodeRow>, VivariumError> {
        if let Some(node) = self.by_source.get(token) {
            return Ok(Some(node));
        }
        let Ok(message_id) = storage.resolve_message_token(token) else {
            return Ok(None);
        };
        let Some(view) = storage.message_by_id(&message_id)? else {
            return Ok(None);
        };
        let basis = view
            .message_id
            .strip_prefix("msg_")
            .unwrap_or(&view.message_id);
        Ok(self
            .lengths
            .iter()
            .filter_map(|len| basis.get(..*len))
            .find_map(|prefix| self.by_source.get(prefix)))
    }

    /// The nodes in the subgraph named by `parent_source_id` (a need's
    /// bound units), in source-id order.
    pub(super) fn subgraph_nodes(&self, parent_source_id: &str) -> Vec<&WorkGraphNodeRow> {
        self.by_subgraph
            .get(parent_source_id)
            .map(|ids| ids.iter().filter_map(|id| self.by_source.get(id)).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "backlog_index_test.rs"]
mod tests;
