//! Reference follow: after a pull, look up the letters its new letters point
//! at and the mailspace does not hold.
//!
//! A reference is a value of the `depends_on` front-matter key, or a whole-word
//! 8-hex token in the subject or body. Each further hop scans only what the
//! previous hop filed. A token that matches nothing is ignored (counted).

use std::collections::HashSet;

use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::storage::Storage;
use crate::sync::fetch::lookup_and_file;
use crate::sync::filer::Filer;
use crate::sync::pull::{PullReport, settle_events, settle_links};
use crate::sync::state::SyncState;
use crate::sync::transport::Transport;
use crate::sync::{SyncSettings, sync_dir};

/// Deepest hop count a pull may follow.
pub const MAX_DEPTH: u8 = 3;

/// Handles looked up per invocation, across all hops.
pub const MAX_LOOKUPS: usize = 500;

/// What a reference follow did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RefsReport {
    /// Hops that looked something up.
    pub hops: usize,
    /// Distinct references not held, found by scanning.
    pub found: usize,
    /// Handles sent to the server.
    pub looked_up: usize,
    /// Letters written by the follow.
    pub filed: usize,
    /// Looked-up handles that matched nothing visible.
    pub unmatched: usize,
    /// References left unlooked because the invocation cap was reached.
    pub capped: usize,
}

impl RefsReport {
    /// One-line operator summary.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "sync refs: {} hop(s), {} reference(s) not held, {} looked up, {} filed, {} matched \
             nothing, {} over the cap",
            self.hops, self.found, self.looked_up, self.filed, self.unmatched, self.capped
        )
    }
}

/// The 8-hex references a letter's bytes carry: `depends_on` front-matter
/// values and whole-word 8-hex tokens in the subject and body, lowercased, in first
/// appearance order, without duplicates.
#[must_use]
pub fn references_in(bytes: &[u8]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let Some(parsed) = mail_parser::MessageParser::default().parse(bytes) else {
        return found;
    };
    let mut add = |token: String| {
        if !found.contains(&token) {
            found.push(token);
        }
    };
    for value in crate::mailspace::depends_on_refs(bytes) {
        if let Some(handle) = depends_on_handle(&value) {
            add(handle);
        }
    }
    let mut texts = vec![parsed.subject().unwrap_or_default().to_string()];
    texts.extend(
        parsed
            .text_bodies()
            .filter_map(|part| part.text_contents().map(String::from)),
    );
    for text in &texts {
        for word in text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
            if is_handle(word) {
                add(word.to_ascii_lowercase());
            }
        }
    }
    found
}

fn is_handle(word: &str) -> bool {
    word.len() == 8 && word.chars().all(|c| c.is_ascii_hexdigit())
}

/// A dependency value as a handle: 8 hex characters, or the first 8 of a
/// longer hex id (`msg_` prefix allowed).
fn depends_on_handle(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let hex = trimmed.strip_prefix("msg_").unwrap_or(trimmed);
    let head = hex.get(..8)?;
    (hex.chars().all(|c| c.is_ascii_hexdigit()) && is_handle(head))
        .then(|| head.to_ascii_lowercase())
}

/// The 8-hex prefixes of the message ids this mailspace holds live.
fn held_handles(storage: &Storage) -> Result<HashSet<String>, VivariumError> {
    Ok(storage
        .handle_map()?
        .keys()
        .filter_map(|id| id.strip_prefix("msg_").and_then(|rest| rest.get(..8)))
        .map(str::to_ascii_lowercase)
        .collect())
}

/// Refuse a depth outside 1 to [`MAX_DEPTH`].
///
/// # Errors
/// Returns an error naming the allowed range.
pub fn check_depth(depth: u8) -> Result<(), VivariumError> {
    if depth == 0 || depth > MAX_DEPTH {
        return Err(VivariumError::Message(format!(
            "--depth must be 1 to {MAX_DEPTH}"
        )));
    }
    Ok(())
}

/// Follow references from the letters `pull` filed (`pull.filed_ids`), up to
/// `depth` hops, filing what the server returns into the same report.
///
/// # Errors
/// Returns an error for a depth outside 1 to 3, a failed lookup, or a
/// storage or state failure.
pub fn follow_refs(
    mailspace: &Mailspace,
    settings: &SyncSettings,
    transport: &dyn Transport,
    pull: &mut PullReport,
    depth: u8,
) -> Result<RefsReport, VivariumError> {
    check_depth(depth)?;
    let storage = mailspace.storage()?;
    let filer = Filer::new(mailspace, settings);
    let mut report = RefsReport::default();
    let mut pending = Vec::new();
    let mut scan: Vec<String> = pull.filed_ids.clone();
    let mut asked: HashSet<String> = HashSet::new();
    for _ in 0..depth {
        let wanted = unheld_references(&storage, &scan, &asked)?;
        report.found += wanted.len();
        let room = MAX_LOOKUPS.saturating_sub(report.looked_up);
        let (now, later) = wanted.split_at(wanted.len().min(room));
        report.capped += later.len();
        if now.is_empty() {
            break;
        }
        asked.extend(now.iter().cloned());
        report.hops += 1;
        report.looked_up += now.len();
        let mark = pull.filed_ids.len();
        let missing = lookup_and_file(transport, &storage, &filer, now, pull, &mut pending)?;
        report.unmatched += missing.len();
        scan = pull.filed_ids[mark..].to_vec();
        report.filed += scan.len();
        if scan.is_empty() {
            break;
        }
    }
    let dir = sync_dir(&mailspace.dir);
    let mut state = SyncState::load(&dir, &settings.project)?;
    let before = state.clone();
    pull.linked += settle_links(&storage, pending, &mut state)?;
    settle_events(mailspace, pull)?;
    if state != before {
        state.save(&dir)?;
    }
    Ok(report)
}

/// References in the letters behind `ids` that this mailspace does not hold
/// and has not already asked for, in first appearance order.
fn unheld_references(
    storage: &Storage,
    ids: &[String],
    asked: &HashSet<String>,
) -> Result<Vec<String>, VivariumError> {
    let held = held_handles(storage)?;
    let mut wanted: Vec<String> = Vec::new();
    for id in ids {
        let bytes = storage.read_message(id)?;
        for token in references_in(&bytes) {
            if !held.contains(&token) && !asked.contains(&token) && !wanted.contains(&token) {
                wanted.push(token);
            }
        }
    }
    Ok(wanted)
}

#[cfg(test)]
#[path = "refs_test.rs"]
mod tests;
