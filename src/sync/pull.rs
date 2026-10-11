//! Pull: read the project's letters after the stored cursor and file the ones
//! this mailspace does not hold, byte for byte.
//!
//! Every record is verified (`sha256(bytes) == content_sha256`) before it is
//! looked at. Ingest is idempotent and the cursor moves only after a whole
//! page is handled, so a failure replays the page and changes nothing twice.

use std::collections::BTreeMap;

use crate::chat::b64;
use crate::chat::item;
use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::storage::{
    IngestEvent, IngestOutcome, MessageIngestRequest, RemoteBindingInput, Storage,
    StoredMessageView, is_traveling_id, sha256_hex,
};
use crate::sync::link::{self, LetterHeaders, LinkOutcome};
use crate::sync::state::SyncState;
use crate::sync::transport::{Record, Transport};
use crate::sync::{PROVIDER, SyncSettings, UIDVALIDITY, sync_dir};

/// A record this pass would not file, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedRecord {
    pub message_id: String,
    pub reason: String,
}

/// What one pull did, for the operator.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PullReport {
    pub fetched: usize,
    /// New letters filed, by local role.
    pub filed: BTreeMap<String, usize>,
    /// Letters this mailspace already held; left untouched.
    pub existing: usize,
    /// Reply links recorded.
    pub linked: usize,
    pub refused: Vec<RefusedRecord>,
    /// Problems that did not stop the pull (a binding that could not be
    /// written).
    pub warnings: Vec<String>,
    pub cursor: u64,
}

impl PullReport {
    /// One-line operator summary.
    #[must_use]
    pub fn summary(&self) -> String {
        let filed: usize = self.filed.values().sum();
        let by_role = self
            .filed
            .iter()
            .map(|(role, count)| format!("{count} {role}"))
            .collect::<Vec<_>>()
            .join(", ");
        let new = if by_role.is_empty() {
            format!("{filed} new")
        } else {
            format!("{filed} new ({by_role})")
        };
        let refused = if self.refused.is_empty() {
            "0 refused".to_string()
        } else {
            let shown = self.refused.iter().take(5).map(|r| r.message_id.as_str());
            let ids = shown.collect::<Vec<_>>().join(", ");
            let more = self.refused.len().saturating_sub(5);
            let tail = if more > 0 {
                format!(", ... and {more} more")
            } else {
                String::new()
            };
            format!("{} refused: {ids}{tail}", self.refused.len())
        };
        format!(
            "sync pull: {} fetched, {new}, {} already present, {} reply link(s), {refused}, \
             cursor {}",
            self.fetched, self.existing, self.linked, self.cursor
        )
    }

    /// Why the command should exit non-zero, if it should.
    #[must_use]
    pub fn failure(&self) -> Option<String> {
        (!self.refused.is_empty()).then(|| {
            format!(
                "{} record(s) refused (hash mismatch, id clash, or bad id); see above",
                self.refused.len()
            )
        })
    }
}

/// Pull everything after the stored cursor, page by page.
///
/// # Errors
/// Returns an error when `[sync] recipient` is not resolved, the read fails,
/// or storage or the state file cannot be written.
pub fn pull(
    mailspace: &Mailspace,
    settings: &SyncSettings,
    transport: &dyn Transport,
) -> Result<PullReport, VivariumError> {
    let recipient = settings.recipient.as_deref().ok_or_else(|| {
        VivariumError::Config("[sync] recipient is not set; pull files letters under it".into())
    })?;
    let dir = sync_dir(&mailspace.dir);
    let mut state = SyncState::load(&dir, &settings.project)?;
    let mut storage = mailspace.storage()?;
    let filer = Filer {
        mailspace,
        settings,
        recipient,
    };
    let mut report = PullReport::default();
    loop {
        let page = transport.fetch(state.since)?;
        report.fetched += page.records.len();
        let before = state.clone();
        let mut pending = Vec::new();
        for record in &page.records {
            filer.file_record(&mut storage, record, &mut report, &mut pending)?;
        }
        report.linked += settle_links(&storage, pending, &mut state)?;
        let advanced = !page.records.is_empty() && page.next_cursor > state.since;
        if advanced {
            state.since = page.next_cursor;
        }
        if state != before {
            state.save(&dir)?;
        }
        if !advanced {
            break;
        }
    }
    report.cursor = state.since;
    Ok(report)
}

/// Try every reply this page produced, and every reply still waiting from
/// earlier pulls, against the rows now held. A reply whose parent is still
/// absent stays in the state for the next pull.
fn settle_links(
    storage: &Storage,
    pending: Vec<(String, String)>,
    state: &mut SyncState,
) -> Result<usize, VivariumError> {
    let mut work = std::mem::take(&mut state.dangling);
    work.extend(pending);
    let mut linked = 0;
    for (child, parent) in work {
        match link::link_reply(storage, &child, &parent)? {
            LinkOutcome::Linked => linked += 1,
            LinkOutcome::Settled => {}
            LinkOutcome::ParentMissing => {
                state.dangling.insert(child, parent);
            }
        }
    }
    Ok(linked)
}

/// The fixed inputs of filing one record.
struct Filer<'a> {
    mailspace: &'a Mailspace,
    settings: &'a SyncSettings,
    recipient: &'a str,
}

impl Filer<'_> {
    /// Verify one record, then file it or note that it is already held.
    /// `pending` collects `(child content id, parent RFC id)` for the link
    /// pass.
    fn file_record(
        &self,
        storage: &mut Storage,
        record: &Record,
        report: &mut PullReport,
        pending: &mut Vec<(String, String)>,
    ) -> Result<(), VivariumError> {
        let Some(bytes) = b64::decode(&record.raw_eml) else {
            refuse(report, record, "raw_eml is not valid base64");
            return Ok(());
        };
        let content_id = sha256_hex(&bytes);
        if !content_id.eq_ignore_ascii_case(&record.content_sha256) {
            refuse(report, record, "bytes do not match content_sha256");
            return Ok(());
        }
        let headers = link::parse_headers(&bytes);
        let held = storage.live_messages_for_content(&content_id)?;
        if held.is_empty() {
            if !self.file_new(storage, record, &bytes, &headers, report)? {
                return Ok(());
            }
        } else {
            report.existing += 1;
            self.bind_held(storage, record, &held, report);
        }
        if let Some(parent) = headers.parent {
            pending.push((content_id, parent));
        }
        Ok(())
    }

    /// Ingest a letter nothing holds. Returns whether it was filed or found
    /// held; `false` means it was refused.
    fn file_new(
        &self,
        storage: &mut Storage,
        record: &Record,
        bytes: &[u8],
        headers: &LetterHeaders,
        report: &mut PullReport,
    ) -> Result<bool, VivariumError> {
        if !is_traveling_id(&record.message_id) {
            refuse(report, record, "id is not a traveling id");
            return Ok(false);
        }
        let Ok(uid) = u32::try_from(record.sequence) else {
            refuse(report, record, "sequence does not fit a remote binding");
            return Ok(false);
        };
        let own = record.from == self.settings.name;
        let role = item::role_for(record.kind.as_deref(), own);
        let request = self.request(record, role, own, uid);
        let note = format!(
            "project {} item {} from {}",
            self.settings.project, record.sequence, record.from
        );
        let event = IngestEvent {
            command: "sync pull",
            event_type: if own {
                "sent_copy_created"
            } else {
                "delivered"
            },
            subject: &headers.subject,
            note: Some(&note),
        };
        let (outcome, stored) = storage.ingest_if_absent(&request, bytes, Some(&event))?;
        match outcome {
            IngestOutcome::Inserted | IngestOutcome::Restored => {
                *report.filed.entry(role.to_string()).or_default() += 1;
                if let (Some(kind), false) = (item::work_kind(record.kind.as_deref()), own) {
                    let handle = storage.display_handle(&stored.message_id)?;
                    self.mailspace
                        .backlog_attach(kind, &handle, &headers.subject, &[])?;
                }
                Ok(true)
            }
            IngestOutcome::Existing => {
                report.existing += 1;
                Ok(true)
            }
            IngestOutcome::Conflict => {
                refuse(report, record, "id already names different content here");
                Ok(false)
            }
        }
    }

    fn request(&self, record: &Record, role: &str, own: bool, uid: u32) -> MessageIngestRequest {
        MessageIngestRequest {
            account: self.recipient.to_string(),
            local_role: role.to_string(),
            read_state: own,
            starred: false,
            message_id_hint: None,
            presented_id: Some(record.message_id.clone()),
            seed_hint: format!("{PROVIDER}\0{}\0{}", self.settings.project, record.sequence),
            remote: Some(self.binding(self.recipient, uid)),
        }
    }

    fn binding(&self, account: &str, uid: u32) -> RemoteBindingInput {
        RemoteBindingInput {
            account: account.to_string(),
            provider: PROVIDER.to_string(),
            remote_mailbox: self.settings.project.clone(),
            remote_uid: uid,
            remote_uidvalidity: UIDVALIDITY,
        }
    }

    /// A held letter nothing binds to this project learns its server
    /// position, so the next push does not send it again. A failure here is
    /// a warning: the letter is already filed.
    fn bind_held(
        &self,
        storage: &mut Storage,
        record: &Record,
        held: &[StoredMessageView],
        report: &mut PullReport,
    ) {
        let bound = held.iter().any(|row| {
            row.remote.as_ref().is_some_and(|remote| {
                remote.provider == PROVIDER && remote.remote_mailbox == self.settings.project
            })
        });
        let Ok(uid) = u32::try_from(record.sequence) else {
            return;
        };
        let free = |row: &&StoredMessageView| row.remote.is_none();
        let target = held
            .iter()
            .filter(free)
            .find(|row| row.message_id == record.message_id)
            .or_else(|| held.iter().find(free));
        let Some(target) = target.filter(|_| !bound) else {
            return;
        };
        let binding = self.binding(&target.account, uid);
        if let Err(error) = storage.bind_remote(&target.message_id, &binding) {
            report
                .warnings
                .push(format!("could not bind {}: {error}", target.handle));
        }
    }
}

fn refuse(report: &mut PullReport, record: &Record, reason: &str) {
    report.refused.push(RefusedRecord {
        message_id: record.message_id.clone(),
        reason: reason.to_string(),
    });
}

#[cfg(test)]
#[path = "pull_test.rs"]
mod tests;
