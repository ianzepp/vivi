//! Whose work a backlog node is.
//!
//! Every need, task and want this mailspace files or sends becomes a node in
//! the backlog graph, but only some of them are this mailspace's to dispatch.
//! A row falls in one of three holdings:
//!
//! - **local**: a role of this mailspace holds it;
//! - **foreign**: another mailspace's role holds it, so the row sits under the
//!   raw address it was written to (a letter between two other members);
//! - **unowned**: it was addressed to a role of this mailspace that does not
//!   exist (a typo, a retired name never added), so the row sits under the raw
//!   role name until `role add` adopts it;
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
/// Readiness of an open or active node whose work a role reported done and
/// nobody who may accept it has accepted.
pub(crate) const READINESS_REPORTED: &str = "reported";
/// Readiness of an open node held for another mailspace's role.
pub(crate) const READINESS_FOREIGN: &str = "foreign";
/// Readiness of an open node held for a role this mailspace does not have.
pub(crate) const READINESS_UNOWNED: &str = "unowned";

/// How this mailspace holds the row behind one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Holding {
    Local,
    /// Held under another mailspace's address (the account it is filed under).
    Foreign(String),
    /// Held under a role name this mailspace does not have.
    Unowned(String),
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

/// The first bare lowercase address of a stored address list.
pub(super) fn first_bare_address(list: &str) -> Option<String> {
    bare_addresses(list).into_iter().next()
}

fn recipients(view: &StoredMessageView) -> Vec<String> {
    let mut all = bare_addresses(&view.to_addr);
    all.extend(bare_addresses(&view.cc_addr));
    all
}

impl Mailspace {
    /// Whether `account` is a role name that this mailspace never added: a
    /// letter addressed to one of its own roles that does not exist.
    #[must_use]
    pub fn is_unowned_account(&self, account: &str) -> bool {
        !is_foreign_account(account) && !self.is_role_account(account)
    }

    /// Whether `account` is a role (or former role name) of this mailspace,
    /// so its letters are this mailspace's to read and dispatch.
    #[must_use]
    pub fn is_local_account(&self, account: &str) -> bool {
        !is_foreign_account(account) && self.is_role_account(account)
    }

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

    /// Whether every To and Cc address of `view` names another mailspace, so
    /// the sender's own copy is the only row this mailspace holds of it.
    pub(super) fn all_recipients_remote(&self, view: &StoredMessageView) -> bool {
        let all = recipients(view);
        !all.is_empty() && self.remote_recipients(view).len() == all.len()
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

    pub(super) fn holding_of(
        &self,
        storage: &Storage,
        view: &StoredMessageView,
    ) -> Result<Holding, VivariumError> {
        if let Some(holding) = self.assigned_holding(storage, view)? {
            return Ok(holding);
        }
        if is_foreign_account(&view.account) {
            return Ok(Holding::Foreign(view.account.clone()));
        }
        if self.is_unowned_account(&view.account) {
            return Ok(Holding::Unowned(view.account.clone()));
        }
        match self.sent_copy_recipient(view) {
            Some(address) => Ok(Holding::Sent(address)),
            None => Ok(Holding::Local),
        }
    }

    /// The holding of the row a node's source id names. A source id that no
    /// longer resolves is local: the audit reports it, not the projection.
    pub(super) fn node_holding(
        &self,
        storage: &Storage,
        source_id: &str,
    ) -> Result<Holding, VivariumError> {
        match node_row(storage, source_id)? {
            Some(view) => self.holding_of(storage, &view),
            None => Ok(Holding::Local),
        }
    }

    /// Mark the nodes this mailspace does not dispatch: foreign ones read
    /// `foreign`; a sent copy that would be ready reads `awaiting`. Work a
    /// role reported done and nobody has accepted reads `reported`, open or
    /// active alike.
    pub(super) fn mark_holdings(
        &self,
        storage: &Storage,
        nodes: &mut [GraphNodeView],
    ) -> Result<(), VivariumError> {
        for node in nodes
            .iter_mut()
            .filter(|node| matches!(node.state.as_str(), "open" | "active"))
        {
            let row = node_row(storage, &node.source_id)?;
            let holding = match row.as_ref() {
                Some(view) => self.holding_of(storage, view)?,
                None => Holding::Local,
            };
            if let Some(row) = &row
                && matches!(holding, Holding::Local | Holding::Sent(_))
                && self.report_of(storage, row)?.is_some()
            {
                node.readiness = READINESS_REPORTED.into();
                continue;
            }
            if node.state != "open" {
                continue;
            }
            match holding {
                Holding::Local => {}
                Holding::Foreign(_) => node.readiness = READINESS_FOREIGN.into(),
                Holding::Unowned(_) => node.readiness = READINESS_UNOWNED.into(),
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
            Holding::Unowned(account) => {
                return Err(VivariumError::Message(format!(
                    "cannot activate '{source_id}': it is held for the unknown role '{account}'; \
                     add the role with `vivi role add {account}` first"
                )));
            }
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

/// The row a node's source id names, when it still resolves.
pub(super) fn node_row(
    storage: &Storage,
    source_id: &str,
) -> Result<Option<StoredMessageView>, VivariumError> {
    let Ok(message_id) = storage.resolve_message_token(source_id) else {
        return Ok(None);
    };
    storage.message_by_id(&message_id)
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
