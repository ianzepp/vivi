//! Filing one pulled record: verify it, then file it by address.
//!
//! Shared by `pull`, `fetch`, and reference follow, so every route a letter
//! can arrive by gets the same checks: the bytes hash to `content_sha256`,
//! the envelope recipients equal the letter's own To and Cc, and a
//! prefix-shaped From domain carries the prefix the server stamped for the
//! sender. Filing is by address and additive (see [`crate::sync::pull`]).

use std::collections::BTreeSet;

use crate::b64;
use crate::error::VivariumError;
use crate::mailspace::{Mailspace, prefix_of_domain};
use crate::storage::{
    IngestEvent, IngestOutcome, MessageIngestRequest, RemoteBindingInput, Storage,
    StoredMessageView, is_traveling_id, sha256_hex,
};
use crate::sync::link::{self, LetterHeaders};
use crate::sync::pull::{PullReport, RefusedRecord};
use crate::sync::transport::Record;
use crate::sync::{PROVIDER, SyncSettings, UIDVALIDITY};

/// The inputs of filing one record. The validity bindings are written
/// under starts at [`UIDVALIDITY`] and is kept in step with the server
/// epoch by the pull (see [`Filer::set_validity`]).
pub(crate) struct Filer<'a> {
    mailspace: &'a Mailspace,
    settings: &'a SyncSettings,
    validity: u32,
}

/// Where one row of a letter goes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Placement {
    account: String,
    role: &'static str,
    /// Whether the row takes the letter's server position. A second row
    /// under the same account cannot: the binding model holds one row per
    /// account and position.
    bind: bool,
    /// Whether the row becomes a backlog node (work kinds only).
    attach: bool,
    /// The account is a role name this mailspace does not have: the row is
    /// held under the raw name and mints no node until the role is added.
    unknown_role: bool,
}

impl Placement {
    fn is_sent(&self) -> bool {
        self.role == "sent"
    }

    /// A delivered row for a local part of this mailspace's domain; a role
    /// that is not there yet holds the row without a node.
    fn delivered(account: String, role: &'static str, known: bool) -> Self {
        Self {
            account,
            role,
            bind: true,
            attach: known,
            unknown_role: !known,
        }
    }

    /// A row held under another mailspace's address: no node, no role.
    fn held(account: String, role: &'static str) -> Self {
        Self {
            account,
            role,
            bind: true,
            attach: false,
            unknown_role: false,
        }
    }
}

impl<'a> Filer<'a> {
    pub(crate) fn new(mailspace: &'a Mailspace, settings: &'a SyncSettings) -> Self {
        Self {
            mailspace,
            settings,
            validity: UIDVALIDITY,
        }
    }

    /// Set the validity bindings are written under: the epoch the current
    /// pull has just seen for the first time or reset to. A wiped and
    /// restored server reuses sequence numbers, so the epoch it mints for
    /// the new log is the validity that keeps the old log's bindings apart.
    pub(crate) fn set_validity(&mut self, validity: u32) {
        self.validity = validity;
    }

    /// Verify one record, then file it or note that it is already held.
    /// `pending` collects `(child content id, parent RFC id)` for the link
    /// pass.
    pub(crate) fn file_record(
        &self,
        storage: &Storage,
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
        if !envelope_agrees(record.to.as_deref(), &headers) {
            refuse(report, record, ENVELOPE_MISMATCH);
            return Ok(());
        }
        if let Some(reason) = forged_from(record.from_prefix.as_deref(), &headers) {
            refuse(report, record, &reason);
            return Ok(());
        }
        if is_governance(record, &headers) {
            report.governance += 1;
            return Ok(());
        }
        let held = storage.live_messages_for_content(&content_id)?;
        if held.is_empty() {
            if !self.file_new(storage, record, &bytes, &headers, report)? {
                return Ok(());
            }
        } else {
            report.existing += 1;
            self.bind_held(storage, record, &held, report);
        }
        // Event letters and work items are settled against each other once
        // the page is filed and its reply links are made.
        if headers.is_event || work_kind(record.kind.as_deref()).is_some() {
            report.touched.push(content_id.clone());
        }
        if let Some(parent) = headers.parent {
            pending.push((content_id, parent));
        }
        Ok(())
    }

    /// Ingest a letter nothing holds, one row per placement. Returns whether
    /// it was filed or found held; `false` means it was refused.
    fn file_new(
        &self,
        storage: &Storage,
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
        let placements = self.placements(record, headers, report);
        let mut noted = false;
        let mut arrived = Vec::new();
        for (index, placement) in placements.iter().enumerate() {
            // The wire id names the first row (a delivered row when there is
            // one, as at the sender); the others take the id their binding
            // derives, so each is stable across replays.
            let request = self.request(record, placement, uid, index == 0, headers.is_event);
            let (outcome, message_id) =
                self.ingest_row(storage, record, bytes, headers, placement, &request)?;
            match outcome {
                IngestOutcome::Conflict => {
                    refuse(report, record, "id already names different content here");
                    return Ok(false);
                }
                IngestOutcome::Existing if index == 0 => report.existing += 1,
                IngestOutcome::Inserted | IngestOutcome::Restored => {
                    count_written(report, placement, outcome);
                    arrived.push((
                        message_id.clone(),
                        record.content_sha256.to_ascii_lowercase(),
                    ));
                    if !noted {
                        report.filed_ids.push(message_id);
                        noted = true;
                    }
                }
                IngestOutcome::Existing => {}
            }
        }
        // Every row of the letter has its node now: edges that were waiting
        // for this letter can be made.
        self.mailspace
            .backlog_settle_pending_dependencies(&arrived)?;
        Ok(true)
    }

    /// Ingest one row and attach the backlog node for a work item. An event
    /// letter never attaches one: it files as mail whatever kind it carries
    /// (see [`letter_kind`]).
    fn ingest_row(
        &self,
        storage: &Storage,
        record: &Record,
        bytes: &[u8],
        headers: &LetterHeaders,
        placement: &Placement,
        request: &MessageIngestRequest,
    ) -> Result<(IngestOutcome, String), VivariumError> {
        let note = format!(
            "project {} item {} from {}",
            self.settings.project, record.sequence, record.from
        );
        let event = IngestEvent {
            command: "sync pull",
            event_type: if placement.is_sent() {
                "sent_copy_created"
            } else {
                "delivered"
            },
            subject: &headers.subject,
            note: Some(&note),
        };
        let (outcome, stored) = storage.ingest_if_absent(request, bytes, Some(&event))?;
        if let (true, Some(kind), true) = (
            outcome.wrote(),
            work_kind(letter_kind(record, headers)),
            placement.attach,
        ) {
            self.mailspace.backlog_attach_letter(
                kind,
                &stored.message_id,
                &headers.subject,
                bytes,
            )?;
        }
        Ok((outcome, stored.message_id))
    }

    /// The rows a new letter becomes, delivered rows first.
    ///
    /// One delivered row per To or Cc address whose domain is this
    /// mailspace's (own prefix or the legacy name domain), under that local
    /// role and the folder its kind files under. If the From address names
    /// this mailspace as well, a `sent` row under the From role. A letter
    /// with neither is held under its first To address, verbatim, so nothing
    /// is dropped; it is another mailspace's work and mints no backlog node.
    /// An event letter files as mail whatever kind it carries: no work row,
    /// no node, ever.
    fn placements(
        &self,
        record: &Record,
        headers: &LetterHeaders,
        report: &mut PullReport,
    ) -> Vec<Placement> {
        let kind = letter_kind(record, headers);
        let mut placements: Vec<Placement> = Vec::new();
        for address in &headers.recipients {
            let Some((local, domain)) = address.rsplit_once('@') else {
                continue;
            };
            if !self.mailspace.is_own_domain(domain) {
                continue;
            }
            let (account, known) = self.role_account(local, report);
            if !placements.iter().any(|p| p.account == account) {
                let role = role_for(kind);
                placements.push(Placement::delivered(account, role, known));
            }
        }
        let from = headers
            .from
            .as_deref()
            .and_then(|from| from.rsplit_once('@'));
        if let Some((local, _)) = from.filter(|(_, domain)| self.mailspace.is_own_domain(domain)) {
            let (account, known) = self.role_account(local, report);
            // A letter to oneself has delivered and sent rows under one
            // account; only the delivered row takes the server position.
            let bind = !placements.iter().any(|p| p.account == account);
            let attach = placements.is_empty() && known;
            placements.push(Placement {
                account,
                role: "sent",
                bind,
                attach,
                unknown_role: false,
            });
        }
        if placements.is_empty() {
            let account = headers
                .recipients
                .first()
                .cloned()
                .unwrap_or_else(|| record.from.clone());
            // Held for another mailspace's role: the rows and blobs are
            // kept, but the work is not this mailspace's backlog.
            placements.push(Placement::held(account, role_for(kind)));
        }
        placements
    }

    /// The account a local part files under, and whether it is a role here.
    ///
    /// A known role files under its canonical name (a renamed role keeps its
    /// old name as an alias) and warns once per pull when it is retired. An
    /// unknown one files under the name as written, lowercased as a role name
    /// is, with a warning once per name; it is held, not refused.
    fn role_account(&self, local: &str, report: &mut PullReport) -> (String, bool) {
        let (account, known, warning) = match self.mailspace.resolve_identity(local) {
            Ok(role) => {
                let retired = self.mailspace.role_is_retired(&role);
                let warning = retired.then(|| format!("role {role} is retired"));
                (role, true, warning)
            }
            Err(_) => {
                let name = local.to_ascii_lowercase();
                let warning = format!("filed for unknown role {name}");
                (name, false, Some(warning))
            }
        };
        if let Some(warning) = warning
            && !report.warnings.contains(&warning)
        {
            report.warnings.push(warning);
        }
        (account, known)
    }

    fn request(
        &self,
        record: &Record,
        placement: &Placement,
        uid: u32,
        primary: bool,
        event: bool,
    ) -> MessageIngestRequest {
        MessageIngestRequest {
            account: placement.account.clone(),
            local_role: placement.role.to_string(),
            // An item-state event is a record, not mail to answer: it files
            // read, as the sender's own copy does.
            read_state: placement.is_sent() || event,
            starred: false,
            message_id_hint: None,
            presented_id: primary.then(|| record.message_id.clone()),
            seed_hint: format!(
                "{PROVIDER}\0{}\0{}\0{}\0{}",
                self.settings.project, record.sequence, placement.account, placement.role
            ),
            remote: placement
                .bind
                .then(|| self.binding(&placement.account, uid)),
        }
    }

    fn binding(&self, account: &str, uid: u32) -> RemoteBindingInput {
        RemoteBindingInput {
            account: account.to_string(),
            provider: PROVIDER.to_string(),
            remote_mailbox: self.settings.project.clone(),
            remote_uid: uid,
            remote_uidvalidity: self.validity,
        }
    }

    /// A held letter nothing binds to this project learns its server
    /// position, so the next push does not send it again. A failure here is
    /// a warning: the letter is already filed.
    fn bind_held(
        &self,
        storage: &Storage,
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

const ENVELOPE_MISMATCH: &str = "envelope recipients do not match the letter's To/Cc";

/// Whether a letter is project governance: its envelope `to` is the reserved
/// `postmaster`, or its To or Cc names `postmaster` as the local part of any
/// address, case aside. A pull counts such a letter and files it under no
/// role: no delivered row, no sent copy, nothing held.
fn is_governance(record: &Record, headers: &LetterHeaders) -> bool {
    let envelope = record
        .to
        .as_deref()
        .map(str::trim)
        .is_some_and(|to| to.eq_ignore_ascii_case("postmaster"));
    envelope
        || headers
            .recipients
            .iter()
            .any(|address| local_part(address).eq_ignore_ascii_case("postmaster"))
}

/// The part of an address before its `@`, or the whole bare name.
fn local_part(address: &str) -> &str {
    address.rsplit_once('@').map_or(address, |(local, _)| local)
}

/// Whether the envelope's recipient list is the letter's own To and Cc. An
/// absent (or empty) envelope `to`, and `postmaster`, are not checked.
fn envelope_agrees(envelope: Option<&str>, headers: &LetterHeaders) -> bool {
    let Some(declared) = envelope.map(str::trim).filter(|to| !to.is_empty()) else {
        return true;
    };
    if declared.eq_ignore_ascii_case("postmaster") {
        return true;
    }
    let declared: BTreeSet<String> = declared
        .split(',')
        .map(|address| address.trim().to_ascii_lowercase())
        .filter(|address| !address.is_empty())
        .collect();
    let own: BTreeSet<String> = headers.recipients.iter().cloned().collect();
    declared == own
}

/// Why a letter's From address is not its sender's, if it is not.
///
/// A From domain of exactly `<8 lowercase hex>.local` claims a mailspace
/// prefix, and the server stamps every record with the prefix its
/// authenticated sender registered. The two must agree (case aside), else any
/// member could write as another mailspace's role. Other domains (legacy
/// names, external) and letters with no From make no claim.
pub(crate) fn forged_from(stamped: Option<&str>, headers: &LetterHeaders) -> Option<String> {
    let from = headers.from.as_deref()?;
    let (_, domain) = from.rsplit_once('@')?;
    let claimed = prefix_of_domain(&domain.to_ascii_lowercase())?.to_string();
    let stamped = stamped.map(str::to_ascii_lowercase);
    if stamped.as_deref() == Some(claimed.as_str()) {
        return None;
    }
    Some(format!(
        "From address claims mailspace {claimed} but the sender is registered as {}",
        stamped.as_deref().unwrap_or("none")
    ))
}

/// The wire kind a record's letter files under: its envelope kind, unless
/// the letter is an event. An event letter rides its item's kind on the
/// wire so every member that holds the item is served it, but it is a
/// record, not work: it files as plain mail — no `needs`/`tasks`/`wants`
/// row and no backlog node, whatever kind the envelope carries.
fn letter_kind<'a>(record: &'a Record, headers: &LetterHeaders) -> Option<&'a str> {
    if headers.is_event {
        None
    } else {
        record.kind.as_deref()
    }
}

/// The mail kind a record's wire `kind` files under, if it is one the client
/// knows. An unknown future kind degrades to plain mail instead of failing a
/// pull.
fn work_kind(kind: Option<&str>) -> Option<&'static str> {
    match kind?.to_ascii_lowercase().as_str() {
        "need" => Some("need"),
        "task" => Some("task"),
        "want" => Some("want"),
        _ => None,
    }
}

/// The local role a delivered letter files under: the work folder for
/// need/task/want, and `inbox` otherwise.
fn role_for(kind: Option<&str>) -> &'static str {
    match work_kind(kind) {
        Some("need") => "needs",
        Some("task") => "tasks",
        Some("want") => "wants",
        _ => "inbox",
    }
}

/// Count one row written by this run: by folder, and by the role name it is
/// held for when this mailspace has no such role.
fn count_written(report: &mut PullReport, placement: &Placement, outcome: IngestOutcome) {
    *report.filed.entry(placement.role.to_string()).or_default() += 1;
    if placement.unknown_role {
        *report
            .unknown_roles
            .entry(placement.account.clone())
            .or_default() += 1;
    }
    if outcome == IngestOutcome::Restored {
        report.restored += 1;
    }
}

fn refuse(report: &mut PullReport, record: &Record, reason: &str) {
    report.refused.push(RefusedRecord {
        message_id: record.message_id.clone(),
        reason: reason.to_string(),
    });
}

#[cfg(test)]
#[path = "filer_test.rs"]
mod tests;
