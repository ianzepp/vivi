//! A report is not an acceptance.
//!
//! A role that cannot close an item (a hand, an auditor, a head) running
//! `done` reports: its own row moves to `done`, and nothing else does. Only
//! the item's author (or the holder of its sent copy), or a role of kind
//! `mind`, accepts it. Until one of them does, the backlog node stays open or
//! active, its dependents stay blocked, and a bound need's join does not
//! fire.
//!
//! The state is a function of the letters held, never of a clock or a
//! server: an item is *reported* when a row of this mailspace sits in `done`,
//! no authorized `done` event heads the item's chain, and the row is held by a
//! role that could not have accepted it.

use super::Mailspace;
use super::backlog::backlog_node_by_token;
use super::graph::GraphNodeView;
use super::holding::{Holding, is_foreign_account, node_row};
use super::item_events::{HeldItem, ItemEventKind, held_item};
use super::step::StepException;
use crate::error::VivariumError;
use crate::storage::{BACKLOG_GRAPH_CODE, Storage, StoredMessageView};

/// What a `done`/`reopen`-style move did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveOutcome {
    /// The handle of the row the move named.
    pub handle: String,
    /// The handle of the event letter, when the move closed or reopened work
    /// as its author or a mind: the change travels to every holder.
    pub event: Option<String>,
    /// Whether a close or reopen of work stayed a local report: only this
    /// mailspace's copy moved and the owner's state is unchanged.
    pub reported: bool,
    /// Handles of bound needs the move completed through its join, as the
    /// mailspace shows them. Empty unless the join rule fired.
    pub joined_needs: Vec<String>,
}

impl MoveOutcome {
    /// The one-line receipt: `done <handle>; event <id>` for a close that
    /// travelled, `done <handle>; reported` for one that did not.
    #[must_use]
    pub fn receipt(&self, verb: &str) -> String {
        match &self.event {
            Some(event) => format!("{verb} {}; event {event}", self.handle),
            None if self.reported => format!("{verb} {}; reported", self.handle),
            None => format!("{verb} {}", self.handle),
        }
    }
}

/// Why a step exception is raised for reported work.
pub(super) const REASON_AWAITING_ACCEPTANCE: &str = "awaiting_acceptance";

/// An item reported done and not yet accepted.
pub(super) struct Report {
    /// `need`, `task` or `want`.
    pub(super) kind: &'static str,
    /// The roles holding a `done` row they could not accept the item with.
    pub(super) reporters: Vec<String>,
    /// The local role that authored the item, when this mailspace did.
    pub(super) author: Option<String>,
    /// The non-retired roles of kind `mind` here.
    pub(super) minds: Vec<String>,
}

impl Report {
    /// The role to name in the command that accepts the item: its author,
    /// else a mind.
    fn acceptor(&self) -> Option<&str> {
        self.author
            .as_deref()
            .or_else(|| self.minds.first().map(String::as_str))
    }

    /// Who may accept, and how, for the step manifest and refusals.
    fn accept_hint(&self, handle: &str) -> String {
        let others = self
            .minds
            .iter()
            .any(|mind| self.author.as_deref() != Some(mind.as_str()));
        let who = match (&self.author, others) {
            (Some(author), false) => format!("its author '{author}'"),
            (Some(author), true) => format!("its author '{author}' or a role of kind mind"),
            (None, true) => "a role of kind mind".to_string(),
            (None, false) => {
                return format!(
                    "no local role may accept it (no author role here and no role of kind \
                     mind); complete it manually with `vivi graph complete {handle}`"
                );
            }
        };
        let role = self.acceptor().unwrap_or("<role>");
        format!(
            "{who} may accept it: `vivi {} done {handle} --for {role}`",
            self.kind
        )
    }
}

impl Mailspace {
    /// Whether `identity` accepts `item` when it closes it: the item's author
    /// (or holder of its sent copy), or a role of kind `mind` or `operator`.
    /// Unlike [`Mailspace::may_close`], a mind or operator accepts for this
    /// mailspace even when another mailspace authored the item, in which case
    /// its close is a local report that is still this mailspace's acceptance.
    pub(super) fn is_acceptor(&self, item: &HeldItem, identity: &str) -> bool {
        self.item_author(item).as_deref() == Some(identity)
            || self.identity_may_close_for_all(identity)
    }

    /// Whether `row`, in `done`, is held by a role that cannot accept `item`:
    /// a report, not a close.
    pub(super) fn is_report_row(&self, item: &HeldItem, row: &StoredMessageView) -> bool {
        let holder = self
            .identity_owning_account(&row.account)
            .unwrap_or_else(|| row.account.clone());
        !self.is_acceptor(item, &holder)
    }

    /// Keep the backlog node in step with a local folder move. A `done` by a
    /// role that is not an acceptor is a report and completes nothing. A
    /// `done` by an acceptor on a row already in `done` accepts the report
    /// that put it there. Every other move syncs as before.
    pub(super) fn sync_backlog_after_move(
        &self,
        storage: &Storage,
        identity: &str,
        before: &StoredMessageView,
        role: &str,
        handle: &str,
        joined: &mut Vec<String>,
    ) -> Result<(), VivariumError> {
        if role == "done"
            && matches!(
                before.local_role.as_str(),
                "tasks" | "needs" | "wants" | "sent" | "done"
            )
            && let Some(item) = held_item(storage, &before.content_id)?
        {
            let accepts = self.is_acceptor(&item, identity);
            match (before.local_role == "done", accepts) {
                (true, true) => {
                    return self
                        .backlog_complete_item_collect(handle, "via=lifecycle", joined)
                        .map(|_| ());
                }
                (_, false) => return Ok(()),
                (false, true) => {}
            }
        }
        self.sync_backlog_node(&before.local_role, role, handle, joined)
    }

    /// The report behind `row`'s item, when one of this mailspace's own rows
    /// of it sits in `done` with no authorized `done` event at the head of its
    /// chain, held by a role that cannot accept it.
    pub(super) fn report_of(
        &self,
        storage: &Storage,
        row: &StoredMessageView,
    ) -> Result<Option<Report>, VivariumError> {
        // Most nodes have no `done` row at all: one query settles them.
        let rows = storage.live_messages_for_content(&row.content_id)?;
        if !rows
            .iter()
            .any(|r| r.local_role == "done" && !is_foreign_account(&r.account))
        {
            return Ok(None);
        }
        let Some(item) = held_item(storage, &row.content_id)? else {
            return Ok(None);
        };
        if self.item_head(storage, &item)? == Some(ItemEventKind::Done) {
            return Ok(None);
        }
        let mut reporters: Vec<String> = Vec::new();
        for done in rows
            .iter()
            .filter(|r| r.local_role == "done" && !is_foreign_account(&r.account))
        {
            let identity = self
                .identity_owning_account(&done.account)
                .unwrap_or_else(|| done.account.clone());
            if !self.is_acceptor(&item, &identity) && !reporters.contains(&identity) {
                reporters.push(identity);
            }
        }
        if reporters.is_empty() {
            return Ok(None);
        }
        Ok(Some(Report {
            kind: item.kind,
            reporters,
            author: self.item_author(&item),
            minds: self.local_minds(),
        }))
    }

    /// The report behind the local node `source_id` names, if any. A node
    /// held for another mailspace's role or an unknown role is not this
    /// mailspace's to accept.
    pub(super) fn node_report(
        &self,
        storage: &Storage,
        source_id: &str,
    ) -> Result<Option<Report>, VivariumError> {
        let Some(row) = node_row(storage, source_id)? else {
            return Ok(None);
        };
        if !matches!(
            self.holding_of(storage, &row)?,
            Holding::Local | Holding::Sent(_)
        ) {
            return Ok(None);
        }
        self.report_of(storage, &row)
    }

    fn local_minds(&self) -> Vec<String> {
        self.config
            .identities
            .iter()
            .filter(|role| !self.role_is_retired(&role.name) && self.identity_is_mind(&role.name))
            .map(|role| role.name.clone())
            .collect()
    }

    /// The `awaiting_acceptance` exception for a reported node.
    pub(super) fn awaiting_acceptance(
        &self,
        storage: &Storage,
        node: &GraphNodeView,
    ) -> Result<StepException, VivariumError> {
        let (kind, detail) = match self.node_report(storage, &node.source_id)? {
            Some(report) => (
                report.kind.to_string(),
                format!(
                    "reported done by {}; not accepted yet, so it stays open and its \
                     dependents stay blocked; {}",
                    report.reporters.join(", "),
                    report.accept_hint(&node.source_id)
                ),
            ),
            None => (
                node.kind.clone(),
                "reported done and not accepted yet".to_string(),
            ),
        };
        Ok(StepException {
            node: node.handle.clone(),
            item: node.source_id.clone(),
            kind,
            reason: REASON_AWAITING_ACCEPTANCE.into(),
            detail,
        })
    }

    /// Whether `step --apply <display>` names an item the manifest already
    /// lists as awaiting acceptance: apply never completes such a node.
    pub(super) fn apply_awaits_acceptance(
        &self,
        display: &str,
        exceptions: &[StepException],
    ) -> Result<bool, VivariumError> {
        let storage = self.storage()?;
        let Some(graph) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? else {
            return Ok(false);
        };
        let nodes = storage.work_graph_nodes(&graph.handle)?;
        let Some(node) = backlog_node_by_token(&storage, &nodes, display)? else {
            return Ok(false);
        };
        Ok(exceptions
            .iter()
            .any(|e| e.reason == REASON_AWAITING_ACCEPTANCE && e.item == node.source_id))
    }

    /// Refuse to activate a node whose work a role already reported done:
    /// it needs an acceptance, not a second dispatch.
    pub(super) fn refuse_reported_activation(
        &self,
        storage: &Storage,
        graph_code: &str,
        source_id: &str,
    ) -> Result<(), VivariumError> {
        if graph_code != BACKLOG_GRAPH_CODE {
            return Ok(());
        }
        let Some(report) = self.node_report(storage, source_id)? else {
            return Ok(());
        };
        Err(VivariumError::Message(format!(
            "cannot activate '{source_id}': it was reported done by {} and awaits acceptance; \
             {}",
            report.reporters.join(", "),
            report.accept_hint(source_id)
        )))
    }

    /// Whether the audit should treat a `done` folder with an open node as a
    /// report awaiting acceptance rather than drift.
    pub(super) fn audit_sees_report(
        &self,
        storage: &Storage,
        content_id: &str,
    ) -> Result<bool, VivariumError> {
        let rows = storage.live_messages_for_content(content_id)?;
        let Some(first) = rows.first() else {
            return Ok(false);
        };
        Ok(self.report_of(storage, first)?.is_some())
    }
}

#[cfg(test)]
#[path = "report_test.rs"]
mod tests;
