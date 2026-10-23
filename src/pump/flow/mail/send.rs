//! Writing half of the mailspace port (send, bind, reply).
//!
//! These are inherent methods of [`MailspacePort`]. The `MailWriter` impl for the
//! port is one block, so it is assembled where the trait lives and delegates
//! here and to the closing half.

use super::MailspacePort;
use crate::error::VivariumError;
use crate::mailspace::SendRequest;

impl MailspacePort {
    /// Send a step task from `from` to `to`, blocked on `depends_on`; returns
    /// the task's handle (the recipient's copy, the one the backlog tracks).
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when a dependency is unknown, an identity does
    /// not resolve, or delivery fails.
    pub fn send_step(
        &self,
        from: &str,
        to: &str,
        subject: &str,
        body: &str,
        depends_on: &[String],
    ) -> Result<String, VivariumError> {
        let result = self.mailspace.send(SendRequest {
            from: from.into(),
            to: vec![to.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: body.into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: depends_on.to_vec(),
        })?;
        Ok(result
            .delivered
            .first()
            .map_or(result.sent, |copy| copy.handle.clone()))
    }

    /// Bind `units` to `need` with one `bound` letter naming all of them.
    /// Binding a unit that is already bound to the same need changes nothing.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the need is not open, a unit has no
    /// backlog node, or storage fails.
    pub fn bind(&self, need: &str, units: &[String]) -> Result<(), VivariumError> {
        self.mailspace.backlog_bind_units(need, units)
    }

    /// Post a reply on the task `handle` as the pump identity.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the handle does not resolve or delivery
    /// fails.
    pub fn reply(&self, handle: &str, body: &str) -> Result<(), VivariumError> {
        self.mailspace.reply(
            handle,
            &self.pump_identity,
            Vec::new(),
            Vec::new(),
            None,
            body.into(),
        )?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "send_test.rs"]
mod tests;
