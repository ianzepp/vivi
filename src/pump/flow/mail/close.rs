//! Closing half of the mailspace port (close, reopen, need close, records).
//!
//! These are inherent methods of [`MailspacePort`]; the `MailWriter` impl in
//! `mail.rs` delegates here. Every write is made as the pump identity. The
//! record methods are named `*_pump_record` because `open_record` is already
//! the reader's method on the same type.

use std::collections::BTreeMap;

use super::MailspacePort;
use crate::error::VivariumError;
use crate::mailspace::PumpState;

impl MailspacePort {
    /// Close the task `handle` with `verdict`. A merge also records the
    /// `branch` and `commit` it landed (both or neither).
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the handle does not resolve, the pump
    /// identity may not close it, or storage fails.
    pub fn close_task(
        &self,
        handle: &str,
        verdict: &str,
        branch: Option<&str>,
        commit: Option<&str>,
    ) -> Result<(), VivariumError> {
        let mut metadata = BTreeMap::new();
        metadata.insert("verdict".to_string(), verdict.to_string());
        match (branch, commit) {
            (Some(branch), Some(commit)) => {
                metadata.insert("branch".to_string(), branch.to_string());
                metadata.insert("commit".to_string(), commit.to_string());
            }
            (None, None) => {}
            _ => {
                return Err(VivariumError::Message(
                    "close_task: branch and commit must be given together".into(),
                ));
            }
        }
        self.mailspace.move_item_outcome(
            &self.pump_identity,
            handle,
            "done",
            None,
            "task done",
            Some(&metadata),
        )?;
        Ok(())
    }

    /// Reopen the closed task `handle`.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the handle does not resolve, the pump
    /// identity may not reopen it, or storage fails.
    pub fn reopen_task(&self, handle: &str) -> Result<(), VivariumError> {
        self.mailspace.move_item_outcome(
            &self.pump_identity,
            handle,
            "tasks",
            None,
            "task reopen",
            None,
        )?;
        Ok(())
    }

    /// Close the need with `note`, as the pump identity. While the need has an
    /// open pump record this travels as an event letter to every member
    /// (operator ruling Q7), so close the need before closing its record.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the need is unknown, already closed, or
    /// the pump identity has no authority over it.
    pub fn close_need(&self, need: &str, note: &str) -> Result<(), VivariumError> {
        self.mailspace.move_item_outcome(
            &self.pump_identity,
            need,
            "done",
            Some(note),
            "need done",
            None,
        )?;
        Ok(())
    }

    /// File the pump record on `need` with `body`; returns its handle.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the need already has an open record or
    /// cannot reach the pump.
    pub fn open_pump_record(&self, need: &str, body: &str) -> Result<String, VivariumError> {
        Ok(self.mailspace.pump_send(None, need, Some(body))?.handle)
    }

    /// Close the open pump record on `need` with `outcome`.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the need has no open record or storage
    /// fails.
    pub fn close_pump_record(&self, need: &str, outcome: &str) -> Result<(), VivariumError> {
        let record = self
            .mailspace
            .pump_list(Some(need), Some(PumpState::Open))?
            .into_iter()
            .next()
            .ok_or_else(|| {
                VivariumError::Message(format!("need {need} has no open pump record"))
            })?;
        self.mailspace.pump_done(&record.handle, Some(outcome))?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "close_test.rs"]
mod tests;
