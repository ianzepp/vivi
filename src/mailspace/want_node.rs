//! The backlog node of a want follows the want's computed state. A want has
//! no folder move and no letter: when its last serving need closes the node
//! completes, and when a serving need opens or reopens the node reopens, so
//! a task that depends on the want is ready exactly while the want is done.

use super::Mailspace;
use super::backlog::backlog_node_by_token;
use super::want_state::{WantState, state_of_handle};
use crate::error::VivariumError;
use crate::storage::BACKLOG_GRAPH_CODE;

impl Mailspace {
    /// Bring the node of the want that the need `need` serves in step with
    /// the want's computed state. A need with no `serves`, a want that is not
    /// held here, or a want with no node changes nothing. Idempotent. A need
    /// that only `explores` a want never reaches it: that key is not read.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub(super) fn settle_served_want(&self, need: &str) -> Result<(), VivariumError> {
        let served = {
            let storage = self.storage()?;
            let Ok(row) = storage.resolve_message_token(need) else {
                return Ok(());
            };
            let Some(view) = storage.message_by_id(&row)? else {
                return Ok(());
            };
            let mut served = None;
            for copy in storage.message_ids_by_content(&view.content_id)? {
                served = storage
                    .item_metadata(&copy)?
                    .get("serves")
                    .filter(|id| !id.is_empty())
                    .cloned();
                if served.is_some() {
                    break;
                }
            }
            served
        };
        let Some(id) = served else {
            return Ok(());
        };
        let Some(want) = self.resolve_want(&format!("<{id}>"))? else {
            return Ok(());
        };
        self.settle_want_node(&want)
    }

    /// Bring the node of the want `want` in step with its computed state:
    /// done when the state is done, open when it is open. A handle that is
    /// not a want node changes nothing. Idempotent. The want's own letter
    /// arriving (send, pull, import) calls this too, so a want that comes in
    /// after its serving needs closed does not sit open.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub(super) fn settle_want_node(&self, want: &str) -> Result<(), VivariumError> {
        let node = {
            let storage = self.storage()?;
            let Some(graph) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? else {
                return Ok(());
            };
            let nodes = storage.work_graph_nodes(&graph.handle)?;
            backlog_node_by_token(&storage, &nodes, want)?
        };
        let Some(node) = node.filter(|node| node.kind == "want") else {
            return Ok(());
        };
        let state = {
            let storage = self.storage()?;
            state_of_handle(&storage, want)?
        };
        match (state, node.state.as_str()) {
            (Some(WantState::Done), "open" | "active") => {
                self.backlog_complete_item_via(want, "via=served")?;
            }
            (Some(WantState::Open), "done") => self.backlog_reopen_item(want)?,
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "want_node_test.rs"]
mod want_node_test;
