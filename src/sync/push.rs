//! Push: send every local letter that has no binding for this project.
//!
//! The stored file goes as-is. A 429, a 401, or a network failure stops the
//! pass (the rest stays unbound for the next run); a refusal of one letter is
//! recorded and the pass goes on. Nothing local is deleted or altered except
//! the remote binding written on an accepted letter.

use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::storage::{RemoteBindingInput, Storage, sha256_hex};
use crate::sync::select::{self, Letter, Selection, Skipped};
use crate::sync::transport::{AppendFailure, Appended, Envelope, Transport};
use crate::sync::{PROVIDER, SyncSettings, UIDVALIDITY};

/// Handles listed per detail group before the rest is summarized.
const LISTED: usize = 10;

/// One letter the server (or the local file check) refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub handle: String,
    pub reason: String,
}

/// A pass cut short, and how much it left unsent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    pub reason: String,
    pub remaining: usize,
}

/// What one push did, for the operator.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PushReport {
    /// Letters the server stored fresh.
    pub sent: usize,
    /// Letters the server already held (a replay, or the same bytes under
    /// another id); bound all the same.
    pub duplicates: usize,
    pub skipped: Vec<Skipped>,
    pub refused: Vec<Refused>,
    /// Accepted letters whose binding could not be written.
    pub unbound: Vec<Refused>,
    pub stopped: Option<Stopped>,
}

impl PushReport {
    /// Operator lines: the summary first, then detail.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![self.summary()];
        if let Some(stopped) = &self.stopped {
            lines.push(format!(
                "stopped: {}; {} letter(s) left unsent",
                stopped.reason, stopped.remaining
            ));
        }
        for (what, list) in [("refused", &self.refused), ("not bound", &self.unbound)] {
            lines.extend(
                list.iter()
                    .take(LISTED)
                    .map(|item| format!("{what} {}: {}", item.handle, item.reason)),
            );
            if list.len() > LISTED {
                lines.push(format!("... and {} more {what}", list.len() - LISTED));
            }
        }
        let actionable: Vec<&Skipped> = self
            .skipped
            .iter()
            .filter(|skipped| skipped.reason.actionable())
            .collect();
        lines.extend(
            actionable
                .iter()
                .take(LISTED)
                .map(|skipped| format!("skipped {}: {}", skipped.handle, skipped.reason.label())),
        );
        if actionable.len() > LISTED {
            lines.push(format!(
                "... and {} more skipped",
                actionable.len() - LISTED
            ));
        }
        lines
    }

    fn summary(&self) -> String {
        let mut policy: Vec<(String, usize)> = Vec::new();
        for skipped in self.skipped.iter().filter(|s| !s.reason.actionable()) {
            let label = skipped.reason.label();
            match policy.iter_mut().find(|(known, _)| *known == label) {
                Some((_, count)) => *count += 1,
                None => policy.push((label, 1)),
            }
        }
        let mut line = format!(
            "sync push: {} sent, {} already on server",
            self.sent, self.duplicates
        );
        let actionable = self
            .skipped
            .iter()
            .filter(|s| s.reason.actionable())
            .count();
        if actionable > 0 {
            line = format!("{line}, {actionable} skipped");
        }
        if !policy.is_empty() {
            let counted = policy
                .iter()
                .map(|(label, count)| format!("{count} {label}"))
                .collect::<Vec<_>>()
                .join(", ");
            line = format!("{line}, not synced by design: {counted}");
        }
        if !self.refused.is_empty() {
            line = format!("{line}, {} refused", self.refused.len());
        }
        if !self.unbound.is_empty() {
            line = format!("{line}, {} not bound", self.unbound.len());
        }
        line
    }

    /// Why the command should exit non-zero, if it should.
    #[must_use]
    pub fn failure(&self) -> Option<String> {
        if let Some(stopped) = &self.stopped {
            return Some(format!(
                "push stopped with {} letter(s) unsent: {}",
                stopped.remaining, stopped.reason
            ));
        }
        let problems = self.refused.len() + self.unbound.len();
        (problems > 0).then(|| format!("{problems} letter(s) refused or not bound; see above"))
    }
}

/// Select and send the letters this mailspace has not pushed.
///
/// # Errors
/// Returns an error on a storage failure. Server refusals and stops are in
/// the report, not errors.
pub fn push(
    mailspace: &Mailspace,
    settings: &SyncSettings,
    transport: &dyn Transport,
) -> Result<PushReport, VivariumError> {
    let mut storage = mailspace.storage()?;
    let selection = select::select(mailspace, &storage, settings)?;
    send_all(&mut storage, settings, transport, selection)
}

fn send_all(
    storage: &mut Storage,
    settings: &SyncSettings,
    transport: &dyn Transport,
    selection: Selection,
) -> Result<PushReport, VivariumError> {
    let mut report = PushReport {
        skipped: selection.skipped,
        ..PushReport::default()
    };
    let total = selection.letters.len();
    for (index, letter) in selection.letters.iter().enumerate() {
        let bytes = storage.read_message(&letter.wire_id)?;
        if sha256_hex(&bytes) != letter.content_id {
            report.refused.push(Refused {
                handle: letter.handle.clone(),
                reason: "the stored file does not match its content id; not sent".into(),
            });
            continue;
        }
        let envelope = Envelope {
            raw_eml: &bytes,
            to: &letter.to,
            kind: &letter.kind,
        };
        match transport.append(&letter.wire_id, &envelope) {
            Ok(appended) => bind_sent(storage, settings, letter, &appended, &mut report)?,
            Err(AppendFailure::Refused(reason)) => report.refused.push(Refused {
                handle: letter.handle.clone(),
                reason,
            }),
            Err(AppendFailure::Stop(reason)) => {
                report.stopped = Some(Stopped {
                    reason,
                    remaining: total - index,
                });
                break;
            }
        }
    }
    Ok(report)
}

/// Record the server position of an accepted letter on the row the wire id
/// names, so the next pass sees it as pushed.
fn bind_sent(
    storage: &mut Storage,
    settings: &SyncSettings,
    letter: &Letter,
    appended: &Appended,
    report: &mut PushReport,
) -> Result<(), VivariumError> {
    let unbound = |reason: String| Refused {
        handle: letter.handle.clone(),
        reason,
    };
    let confirmed = appended
        .content_sha256
        .as_deref()
        .is_some_and(|hash| hash.eq_ignore_ascii_case(&letter.content_id));
    if !confirmed {
        report.unbound.push(unbound(
            "the server's content_sha256 does not match the bytes sent; not bound".into(),
        ));
        return Ok(());
    }
    let Ok(uid) = u32::try_from(appended.sequence) else {
        report.unbound.push(unbound(format!(
            "server sequence {} does not fit a remote binding; not bound",
            appended.sequence
        )));
        return Ok(());
    };
    let binding = RemoteBindingInput {
        account: letter.account.clone(),
        provider: PROVIDER.to_string(),
        remote_mailbox: settings.project.clone(),
        remote_uid: uid,
        remote_uidvalidity: UIDVALIDITY,
    };
    match storage.bind_remote(&letter.wire_id, &binding) {
        Ok(true) => {
            if appended.duplicate {
                report.duplicates += 1;
            } else {
                report.sent += 1;
            }
        }
        Ok(false) => report
            .unbound
            .push(unbound("the local row is gone; not bound".into())),
        Err(error) => report.unbound.push(unbound(format!("not bound: {error}"))),
    }
    Ok(())
}

/// Count the letters a push would send, without touching the network.
///
/// # Errors
/// Returns an error on a storage failure.
pub fn unsent(mailspace: &Mailspace, settings: &SyncSettings) -> Result<usize, VivariumError> {
    let storage = mailspace.storage()?;
    Ok(select::select(mailspace, &storage, settings)?.letters.len())
}

#[cfg(test)]
#[path = "push_test.rs"]
mod tests;
