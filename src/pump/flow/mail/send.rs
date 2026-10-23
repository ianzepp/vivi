//! Writing half of the mailspace port (send, bind, reply).
//!
//! These are inherent methods of [`MailspacePort`]. The `MailWriter` impl for the
//! port is one block, so it is assembled where the trait lives and delegates
//! here and to the closing half.

use super::MailspacePort;
use crate::error::VivariumError;
use crate::mailspace::{Recipient, SendRequest};

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

    /// Post a reply on the task `handle` as the pump identity. The reply goes
    /// to the pump identity itself: step tasks are sent pump to pump, so the
    /// parent has no other recipient, and the sent copy is the thread record.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the handle does not resolve or delivery
    /// fails.
    pub fn reply(&self, handle: &str, body: &str) -> Result<(), VivariumError> {
        self.mailspace.reply(
            handle,
            &self.pump_identity,
            vec![self.pump_identity.clone()],
            Vec::new(),
            None,
            body.into(),
        )?;
        Ok(())
    }

    /// Post a plain note on the need `need`, addressed to the need's own
    /// recipient set (its `From`, `To` and `Cc`, minus the pump identity), so
    /// every member that holds the need receives it. [`MailspacePort::reply`]
    /// goes to the sender only. A need with no one else on it gets no post.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the need does not resolve, cannot be
    /// parsed, or delivery fails.
    pub fn post_to_need_audience(&self, need: &str, body: &str) -> Result<(), VivariumError> {
        let (mut to, mut cc) = self.need_audience(need)?;
        if to.is_empty() {
            std::mem::swap(&mut to, &mut cc);
        }
        if to.is_empty() {
            return Ok(());
        }
        self.mailspace
            .reply(need, &self.pump_identity, to, cc, None, body.into())?;
        Ok(())
    }

    /// The need's `From` and `To` (first list) and `Cc` (second list), each
    /// as a local identity or a remote address, without the pump identity.
    fn need_audience(&self, need: &str) -> Result<(Vec<String>, Vec<String>), VivariumError> {
        let bytes = {
            let storage = self.mailspace.storage()?;
            let id = storage.resolve_message_token(need)?;
            storage.read_message(&id)?
        };
        let parsed = mail_parser::MessageParser::default()
            .parse(&bytes)
            .ok_or_else(|| VivariumError::Parse(format!("need {need}: unreadable message")))?;
        let mut to = Vec::new();
        for header in [parsed.from(), parsed.to()] {
            self.collect_audience(header, &mut to);
        }
        let mut cc = Vec::new();
        self.collect_audience(parsed.cc(), &mut cc);
        cc.retain(|who| !to.contains(who));
        Ok((to, cc))
    }

    fn collect_audience(&self, header: Option<&mail_parser::Address<'_>>, out: &mut Vec<String>) {
        let Some(addresses) = header else { return };
        for address in addresses.iter() {
            let Some(value) = address.address.as_deref() else {
                continue;
            };
            let who = match self.mailspace.classify_recipient(value) {
                Ok(Recipient::Local(identity)) if identity != self.pump_identity => identity,
                Ok(Recipient::Remote(remote)) => remote,
                _ => continue,
            };
            if !out.contains(&who) {
                out.push(who);
            }
        }
    }
}

#[cfg(test)]
#[path = "send_test.rs"]
mod tests;
