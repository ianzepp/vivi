//! Letters held under a role name this mailspace does not have.
//!
//! A letter addressed to `ghost@<own prefix>.local` when no role `ghost`
//! exists is held under the raw name: visible to `boot` and `sync status`,
//! never dispatched, and without a backlog node. Adding the role (or renaming
//! another role to the name) adopts those letters: work gets its node and
//! dependency edges then, so the role's queue is the letters' queue.

use std::collections::HashSet;

use super::Mailspace;
use crate::error::VivariumError;

/// Folders a held letter can wait in: the inbox and the work queues.
const HELD_ROLES: [&str; 4] = ["inbox", "tasks", "needs", "wants"];

impl Mailspace {
    /// Role names letters are held under that this mailspace does not have,
    /// with how many letters wait under each.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub fn held_for_unknown_roles(&self) -> Result<Vec<(String, usize)>, VivariumError> {
        let storage = self.storage()?;
        Ok(storage
            .waiting_letters_by_account()?
            .into_iter()
            .filter(|(account, _)| self.is_unowned_account(account))
            .collect())
    }

    /// Adopt the letters held under `role`, a role name that now exists:
    /// work letters get their backlog node and dependency edges. Returns how
    /// many letters were held under the name.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub fn adopt_held_letters(&self, role: &str) -> Result<usize, VivariumError> {
        let storage = self.storage()?;
        let roles: Vec<String> = HELD_ROLES.iter().map(ToString::to_string).collect();
        let rows = storage.list_messages_by_account_roles(&[role.to_string()], &roles)?;
        let mut letters = HashSet::new();
        let mut arrived = Vec::new();
        for row in rows.iter().filter(|row| row.absorbed_at.is_none()) {
            letters.insert(row.content_id.clone());
            arrived.push((row.message_id.clone(), row.content_id.clone()));
            let kind = match row.local_role.as_str() {
                "tasks" => "task",
                "needs" => "need",
                "wants" => "want",
                _ => continue,
            };
            let letter = storage.read_message(&row.message_id)?;
            self.backlog_attach_letter(kind, &row.message_id, &row.subject, &letter)?;
        }
        drop(storage);
        self.backlog_settle_pending_dependencies(&arrived)?;
        Ok(letters.len())
    }
}

#[cfg(test)]
#[path = "adoption_test.rs"]
mod tests;
