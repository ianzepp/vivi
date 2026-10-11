//! Whose work a backlog node is.
//!
//! Every need, task and want this mailspace files or sends becomes a node in
//! the backlog graph, but only some of them are this mailspace's to dispatch.
//! A row falls in one of three holdings:
//!
//! - **local**: a role of this mailspace holds it;
//! - **foreign**: another mailspace's role holds it, so the row sits under the
//!   raw address it was written to (a letter between two other members);
//! - **sent**: it is the sender's own copy of work addressed to another
//!   mailspace. The node is how the sender orders and tracks work in flight,
//!   and the receiving mailspace dispatches it.
//!
//! The graph projection, `step`, `graph activate`, `boot`, the audit and
//! prune read this one classification.

use std::collections::HashSet;

use super::Mailspace;
use super::graph::GraphNodeView;
use crate::error::VivariumError;
use crate::storage::{BACKLOG_GRAPH_CODE, Storage, StoredMessageView};

/// Readiness of an open node that is the sender's copy of remote work.
pub(crate) const READINESS_AWAITING: &str = "awaiting";
/// Readiness of an open node held for another mailspace's role.
pub(crate) const READINESS_FOREIGN: &str = "foreign";

/// How this mailspace holds the row behind one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Holding {
    Local,
    /// Held under another mailspace's address (the account it is filed under).
    Foreign(String),
    /// The sender's own copy, handed to this address.
    Sent(String),
}

/// Whether `account` is another mailspace's role: a local role is a bare
/// name, and only a letter held under its raw address carries an `@`.
#[must_use]
pub fn is_foreign_account(account: &str) -> bool {
    account.contains('@')
}

/// The bare lowercase addresses of a stored address list
/// (`Name <a@b>, c@d`).
fn bare_addresses(list: &str) -> Vec<String> {
    list.split(',')
        .filter_map(|part| {
            let part = part.trim();
            let address = match (part.rfind('<'), part.rfind('>')) {
                (Some(open), Some(close)) if open < close => &part[open + 1..close],
                _ => part,
            };
            let address = address.trim().to_ascii_lowercase();
            address.contains('@').then_some(address)
        })
        .collect()
}

fn recipients(view: &StoredMessageView) -> Vec<String> {
    let mut all = bare_addresses(&view.to_addr);
    all.extend(bare_addresses(&view.cc_addr));
    all
}

impl Mailspace {
    fn address_is_remote(&self, address: &str) -> bool {
        address
            .rsplit_once('@')
            .is_some_and(|(_, domain)| !self.is_own_domain(domain))
    }

    /// Whether any of the stored address lists names another mailspace.
    pub(super) fn addresses_include_remote(&self, lists: &[&str]) -> bool {
        lists
            .iter()
            .flat_map(|list| bare_addresses(list))
            .any(|address| self.address_is_remote(&address))
    }

    /// The To and Cc addresses of `view` that name another mailspace.
    pub(super) fn remote_recipients(&self, view: &StoredMessageView) -> Vec<String> {
        recipients(view)
            .into_iter()
            .filter(|address| self.address_is_remote(address))
            .collect()
    }

    /// The address `view`'s work was handed to when `view` is the sender's
    /// own copy: a row in `sent`, or the sender's row of a letter addressed
    /// only to another mailspace after it was moved (closed, or reopened
    /// before reopening was refused).
    pub(super) fn sent_copy_recipient(&self, view: &StoredMessageView) -> Option<String> {
        let all = recipients(view);
        let remote = self.remote_recipients(view);
        if view.local_role == "sent" {
            return Some(
                remote
                    .first()
                    .or(all.first())
                    .cloned()
                    .unwrap_or_else(|| "its recipients".into()),
            );
        }
        if remote.is_empty() || remote.len() != all.len() {
            return None;
        }
        let sender = bare_addresses(&view.from_addr).into_iter().next()?;
        let (local, domain) = sender.rsplit_once('@')?;
        if !self.is_own_domain(domain) {
            return None;
        }
        let sender_role = self.identity_owning_account(local)?;
        let holder = self.identity_owning_account(&view.account)?;
        (sender_role == holder).then(|| remote[0].clone())
    }

    pub(super) fn holding_of(&self, view: &StoredMessageView) -> Holding {
        if is_foreign_account(&view.account) {
            return Holding::Foreign(view.account.clone());
        }
        match self.sent_copy_recipient(view) {
            Some(address) => Holding::Sent(address),
            None => Holding::Local,
        }
    }

    /// The holding of the row a node's source id names. A source id that no
    /// longer resolves is local: the audit reports it, not the projection.
    pub(super) fn node_holding(
        &self,
        storage: &Storage,
        source_id: &str,
    ) -> Result<Holding, VivariumError> {
        let Ok(message_id) = storage.resolve_message_token(source_id) else {
            return Ok(Holding::Local);
        };
        Ok(storage
            .message_by_id(&message_id)?
            .map_or(Holding::Local, |view| self.holding_of(&view)))
    }

    /// Mark the open nodes this mailspace does not dispatch: foreign ones
    /// read `foreign`; a sent copy that would be ready reads `awaiting`.
    pub(super) fn mark_holdings(
        &self,
        storage: &Storage,
        nodes: &mut [GraphNodeView],
    ) -> Result<(), VivariumError> {
        for node in nodes.iter_mut().filter(|node| node.state == "open") {
            match self.node_holding(storage, &node.source_id)? {
                Holding::Local => {}
                Holding::Foreign(_) => node.readiness = READINESS_FOREIGN.into(),
                Holding::Sent(_) if node.readiness == "ready" => {
                    node.readiness = READINESS_AWAITING.into();
                }
                Holding::Sent(_) => {}
            }
        }
        Ok(())
    }

    /// Refuse to move the sender's own copy of handed-over work back into a
    /// work queue: reopening or promoting is the receiving mailspace's call,
    /// and the move would land a second owner here.
    pub(super) fn refuse_sent_copy_reopen(
        &self,
        handle: &str,
        before: &StoredMessageView,
        role: &str,
    ) -> Result<(), VivariumError> {
        if !matches!(role, "tasks" | "needs" | "wants") {
            return Ok(());
        }
        match self.sent_copy_recipient(before) {
            Some(to) => Err(VivariumError::Message(format!(
                "{handle} is this mailspace's copy of work handed to {to}; only the receiving \
                 mailspace can reopen or promote it"
            ))),
            None => Ok(()),
        }
    }

    /// Refuse to activate a backlog node this mailspace does not dispatch.
    pub(super) fn refuse_unowned_activation(
        &self,
        storage: &Storage,
        graph_code: &str,
        source_id: &str,
    ) -> Result<(), VivariumError> {
        if graph_code != BACKLOG_GRAPH_CODE {
            return Ok(());
        }
        let reason = match self.node_holding(storage, source_id)? {
            Holding::Local => return Ok(()),
            Holding::Sent(to) => format!("it was handed to {to}"),
            Holding::Foreign(account) => format!("it is held for {account}, another mailspace"),
        };
        Err(VivariumError::Message(format!(
            "cannot activate '{source_id}': {reason}; only the receiving mailspace dispatches it"
        )))
    }

    /// Content ids of the rows behind open or active backlog nodes: the work
    /// still in flight, which a prune must keep whichever folder holds it.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub fn open_work_content_ids(&self) -> Result<HashSet<String>, VivariumError> {
        let storage = self.storage()?;
        let mut ids = HashSet::new();
        let Some(graph) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? else {
            return Ok(ids);
        };
        for node in storage.work_graph_nodes(&graph.handle)? {
            if !matches!(node.state.as_str(), "open" | "active") {
                continue;
            }
            let Ok(message_id) = storage.resolve_message_token(&node.source_id) else {
                continue;
            };
            if let Some(view) = storage.message_by_id(&message_id)? {
                ids.insert(view.content_id);
            }
        }
        Ok(ids)
    }
}

/// The nodes of `nodes` whose readiness is `readiness`, in node order.
pub(super) fn with_readiness(nodes: &[GraphNodeView], readiness: &str) -> Vec<GraphNodeView> {
    nodes
        .iter()
        .filter(|node| node.readiness == readiness)
        .cloned()
        .collect()
}

#[cfg(test)]
#[path = "holding_test.rs"]
mod tests;
