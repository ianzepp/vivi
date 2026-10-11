//! Fetch by handle: look letters up on the server and file them through the
//! same verification and filing as a pull.
//!
//! A handle is the 8 hex characters after `msg_` in a message id. A fetch
//! never moves the cursor or the floor: it adds letters a hand's content
//! references and does not hold. This module has no CLI dependencies, so a
//! later transparent mode can call [`fetch_handles`] to resolve a handle it
//! does not hold.

use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::storage::Storage;
use crate::sync::filer::Filer;
use crate::sync::pull::{PullReport, RefusedRecord, settle_events, settle_links};
use crate::sync::state::SyncState;
use crate::sync::transport::Transport;
use crate::sync::{SyncSettings, sync_dir};

/// Handles per lookup request (the server's maximum).
pub const LOOKUP_CHUNK: usize = 100;

/// What one fetch did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FetchReport {
    /// Distinct handles asked for.
    pub handles: usize,
    /// Per-record outcomes: filed by role, already held, refused, links.
    pub pull: PullReport,
    /// Handles that matched nothing the caller may see.
    pub missing: Vec<String>,
}

impl FetchReport {
    /// One-line operator summary.
    #[must_use]
    pub fn summary(&self) -> String {
        let filed: usize = self.pull.filed.values().sum();
        let mut line = format!(
            "sync fetch: {} handle(s), {filed} filed, {} already held, {} missing",
            self.handles,
            self.pull.existing,
            self.missing.len()
        );
        if !self.missing.is_empty() {
            let shown: Vec<&str> = self.missing.iter().take(10).map(String::as_str).collect();
            line = format!("{line} ({})", shown.join(", "));
        }
        if !self.pull.refused.is_empty() {
            line = format!("{line}, {} refused", self.pull.refused.len());
        }
        if let Some(held) = self.pull.held_note() {
            line = format!("{line}, {held}");
        }
        format!("{line}{}", self.pull.event_note())
    }

    /// Refused records, for the operator.
    #[must_use]
    pub fn refused(&self) -> &[RefusedRecord] {
        &self.pull.refused
    }

    /// Why the command should exit non-zero: only refused records count; a
    /// missing handle is an answer, not a failure.
    #[must_use]
    pub fn failure(&self) -> Option<String> {
        self.pull.failure()
    }
}

/// Validate and normalize handles: 8 hex characters, any case, lowercased and
/// de-duplicated in order.
///
/// # Errors
/// Returns an error naming the first value that is not a handle.
pub fn normalize_handles(values: &[String]) -> Result<Vec<String>, VivariumError> {
    let mut handles: Vec<String> = Vec::new();
    for value in values {
        let handle = value.trim().to_ascii_lowercase();
        let valid = handle.len() == 8 && handle.chars().all(|c| c.is_ascii_hexdigit());
        if !valid {
            return Err(VivariumError::Message(format!(
                "'{value}' is not a handle; use the 8 hex characters of a message id"
            )));
        }
        if !handles.contains(&handle) {
            handles.push(handle);
        }
    }
    Ok(handles)
}

/// Look `handles` up in chunks and file every record that comes back. New
/// replies' parents are collected in `pending`; the ids of letters written
/// are appended to `report.filed_ids`. Returns the handles that matched
/// nothing visible.
///
/// # Errors
/// Returns an error when a lookup fails or storage fails.
pub(crate) fn lookup_and_file(
    transport: &dyn Transport,
    storage: &mut Storage,
    filer: &Filer<'_>,
    handles: &[String],
    report: &mut PullReport,
    pending: &mut Vec<(String, String)>,
) -> Result<Vec<String>, VivariumError> {
    let mut missing = Vec::new();
    for chunk in handles.chunks(LOOKUP_CHUNK) {
        let found = transport.lookup(chunk)?;
        report.fetched += found.records.len();
        for record in &found.records {
            filer.file_record(storage, record, report, pending)?;
        }
        missing.extend(found.missing);
    }
    Ok(missing)
}

/// Fetch letters by handle and file them. The cursor and floor are not
/// touched; reply links that cannot be made yet wait in the state file as
/// they do for a pull.
///
/// # Errors
/// Returns an error for a value that is not a handle, a failed lookup, or a
/// storage or state failure.
pub fn fetch_handles(
    mailspace: &Mailspace,
    settings: &SyncSettings,
    transport: &dyn Transport,
    handles: &[String],
) -> Result<FetchReport, VivariumError> {
    let handles = normalize_handles(handles)?;
    // Before any lookup: an unreadable state file fails the command before it
    // files anything, not after.
    let dir = sync_dir(&mailspace.dir);
    let mut state = SyncState::load(&dir, &settings.project)?;
    let mut storage = mailspace.storage()?;
    let filer = Filer::new(mailspace, settings);
    let mut report = FetchReport {
        handles: handles.len(),
        ..FetchReport::default()
    };
    let mut pending = Vec::new();
    report.missing = lookup_and_file(
        transport,
        &mut storage,
        &filer,
        &handles,
        &mut report.pull,
        &mut pending,
    )?;
    let before = state.clone();
    report.pull.linked += settle_links(&storage, pending, &mut state)?;
    settle_events(mailspace, &mut report.pull)?;
    if state != before {
        state.save(&dir)?;
    }
    report.pull.cursor = state.since;
    Ok(report)
}

#[cfg(test)]
#[path = "fetch_test.rs"]
mod tests;
