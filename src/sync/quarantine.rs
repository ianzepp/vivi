//! Quarantine: hold a letter this vivi cannot read, instead of dropping it.
//!
//! A pull that meets a format-1 letter whose front matter will not parse —
//! a newer member's key this vivi does not know, a broken fence, an unknown
//! format — files the exact bytes under `<project>/.vivi/quarantine/` with a
//! JSON receipt and moves on: the cursor has already advanced, so the letter
//! is held, not dropped and not re-fetched. [`crate::sync::pull::PullReport`]
//! names each newly held letter once; the same content arriving again finds
//! the receipt and is silent.
//!
//! Files only — no table, no state beyond them. [`discard`] moves a held
//! pair under `dropped/`; [`refile`] re-runs the filer on the held bytes
//! (the retry path after an upgrade whose parser accepts what the old one
//! refused). The unrelated v19 `pump` quarantine is `crate::pump`
//! namespace; this is the sync one.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::b64;
use crate::error::VivariumError;
use crate::letter;
use crate::mailspace::{Mailspace, prefix_of_domain};
use crate::storage::sha256_hex;
use crate::sync::link;
use crate::sync::pull;
use crate::sync::state::SyncState;
use crate::sync::transport::Record;
use crate::sync::{SyncSettings, sync_dir};

/// The quarantine home: `<project>/.vivi/quarantine/`.
#[must_use]
pub fn dir(vivi_dir: &Path) -> PathBuf {
    vivi_dir.join("quarantine")
}

/// The receipt beside one held letter. Serialized as
/// `{message_id, from, kind, sequence, reason, at}`; `id` is the file name
/// stem (the content sha256), not a JSON field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldLetter {
    #[serde(skip)]
    pub id: String,
    pub message_id: String,
    /// The wire sender name, as the record carried it.
    pub from: String,
    /// The wire kind the letter traveled under, when it named one.
    pub kind: Option<String>,
    pub sequence: u64,
    /// Why the parser refused the bytes, one line.
    pub reason: String,
    /// When the letter was held (RFC 3339).
    pub at: String,
}

impl HeldLetter {
    /// A receipt for one refused wire record, stamped now.
    #[must_use]
    pub fn new(record: &Record, reason: String) -> Self {
        Self {
            id: String::new(),
            message_id: record.message_id.clone(),
            from: record.from.clone(),
            kind: record.kind.clone(),
            sequence: record.sequence,
            reason,
            at: Utc::now().to_rfc3339(),
        }
    }
}

/// Hold `bytes` with their receipt. Returns whether this call newly held
/// the letter: the same content arriving again finds the receipt and
/// returns `false`, so no caller alarms twice. The receipt is written last
/// (temp file, then rename): its presence is what marks the letter held.
///
/// # Errors
/// Returns an error when the quarantine directory or either file cannot be
/// written. A pull that cannot hold a letter it cannot read propagates the
/// error, so its cursor does not advance past the letter.
pub fn hold(vivi_dir: &Path, bytes: &[u8], held: &HeldLetter) -> Result<bool, VivariumError> {
    let id = sha256_hex(bytes);
    let root = dir(vivi_dir);
    let json = root.join(format!("{id}.json"));
    if json.exists() {
        return Ok(false);
    }
    fs::create_dir_all(&root)?;
    fs::write(root.join(format!("{id}.eml")), bytes)?;
    let raw = serde_json::to_string_pretty(held)
        .map_err(|e| VivariumError::Other(format!("failed to encode quarantine receipt: {e}")))?;
    let tmp = json.with_extension("json.tmp");
    fs::write(&tmp, raw)?;
    fs::rename(&tmp, &json)?;
    Ok(true)
}

/// Every held letter, oldest receipt first, then by id for a stable order.
///
/// # Errors
/// Returns an error when the directory cannot be read or a receipt is
/// corrupt (a corrupt receipt is surfaced, never skipped).
pub fn list(vivi_dir: &Path) -> Result<Vec<HeldLetter>, VivariumError> {
    let root = dir(vivi_dir);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut held = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let (Some(id), Some("json")) = (
            path.file_stem().and_then(|stem| stem.to_str()),
            path.extension().and_then(|ext| ext.to_str()),
        ) else {
            continue;
        };
        held.push(read_receipt(&path, id)?);
    }
    held.sort_by(|a, b| (a.at.as_str(), a.id.as_str()).cmp(&(b.at.as_str(), b.id.as_str())));
    Ok(held)
}

/// The receipt of one held letter.
///
/// # Errors
/// Returns an error when `id` is not a content sha256, nothing is held
/// under it, or its receipt cannot be read.
pub fn held(vivi_dir: &Path, id: &str) -> Result<HeldLetter, VivariumError> {
    let id = checked_id(id)?;
    let path = dir(vivi_dir).join(format!("{id}.json"));
    read_receipt(&path, id).map_err(|error| match error {
        VivariumError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => not_held(id),
        other => other,
    })
}

/// The exact held bytes of `id`.
///
/// # Errors
/// Returns an error when `id` is not a content sha256, nothing is held
/// under it, or the file cannot be read.
pub fn read_bytes(vivi_dir: &Path, id: &str) -> Result<Vec<u8>, VivariumError> {
    let id = checked_id(id)?;
    let path = dir(vivi_dir).join(format!("{id}.eml"));
    fs::read(&path).map_err(|error| match error {
        io if io.kind() == std::io::ErrorKind::NotFound => not_held(id),
        io => io.into(),
    })
}

/// Move one held pair under `dropped/`: the operator's answer to a letter
/// that will never be re-filed. Overwrites a pair already dropped under the
/// same id (the bytes are the content).
///
/// # Errors
/// Returns an error when `id` is not a content sha256, nothing is held
/// under it, or the files cannot be moved.
pub fn discard(vivi_dir: &Path, id: &str) -> Result<(), VivariumError> {
    let id = checked_id(id)?;
    let root = dir(vivi_dir);
    let json = root.join(format!("{id}.json"));
    if !json.exists() {
        return Err(not_held(id));
    }
    let dropped = root.join("dropped");
    fs::create_dir_all(&dropped)?;
    fs::rename(&json, dropped.join(format!("{id}.json")))?;
    let eml = root.join(format!("{id}.eml"));
    if eml.exists() {
        fs::rename(&eml, dropped.join(format!("{id}.eml")))?;
    }
    Ok(())
}

/// What [`refile`] did with a held letter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefileOutcome {
    /// The filer filed the letter; the held pair moved to `dropped/`.
    Filed,
    /// The letter stays held: the parser still refuses these bytes, or the
    /// filer refused the record (an id clash, for example). Carries why.
    StillHeld(String),
}

/// Re-run the filer on one held letter's bytes: the retry after an upgrade
/// whose parser accepts what the old one refused. The transport checks
/// (hash, envelope, stamped prefix) ran when the pull first quarantined the
/// letter, and the server's stamp is not part of the receipt, so the
/// synthesized record carries the From address's own claim as its stamp.
///
/// # Errors
/// Returns an error when the receipt or bytes cannot be read, or storage or
/// the sync state cannot be written.
pub fn refile(
    mailspace: &Mailspace,
    settings: &SyncSettings,
    id: &str,
) -> Result<RefileOutcome, VivariumError> {
    let receipt = held(&mailspace.dir, id)?;
    let bytes = read_bytes(&mailspace.dir, id)?;
    if let Err(error) = letter::of_message(&bytes, &receipt.message_id) {
        return Ok(RefileOutcome::StillHeld(error.to_string()));
    }
    let record = held_record(&bytes, &receipt);
    let dir = sync_dir(&mailspace.dir);
    let mut state = SyncState::load(&dir, &settings.project)?;
    let before = state.clone();
    let report = pull::file_records(mailspace, settings, &mut state, &[record])?;
    if state != before {
        state.save(&dir)?;
    }
    if report.refused.is_empty() {
        discard(&mailspace.dir, id)?;
        Ok(RefileOutcome::Filed)
    } else {
        let reasons = report
            .refused
            .iter()
            .map(|refused| format!("{}: {}", refused.message_id, refused.reason))
            .collect::<Vec<_>>()
            .join("; ");
        Ok(RefileOutcome::StillHeld(reasons))
    }
}

/// The wire record a re-file files the held bytes under.
fn held_record(bytes: &[u8], receipt: &HeldLetter) -> Record {
    let headers = link::parse_headers(bytes);
    let stamped = headers
        .from
        .as_deref()
        .and_then(|from| from.rsplit_once('@'))
        .and_then(|(_, domain)| prefix_of_domain(domain))
        .map(ToString::to_string);
    Record {
        sequence: receipt.sequence,
        message_id: receipt.message_id.clone(),
        from: receipt.from.clone(),
        to: None,
        kind: receipt.kind.clone(),
        content_sha256: sha256_hex(bytes),
        from_prefix: stamped,
        appended_at: None,
        raw_eml: b64::encode(bytes),
    }
}

fn read_receipt(path: &Path, id: &str) -> Result<HeldLetter, VivariumError> {
    let raw = fs::read_to_string(path)?;
    let mut held: HeldLetter = serde_json::from_str(&raw).map_err(|e| {
        VivariumError::Message(format!(
            "quarantine receipt {} is corrupt: {e}",
            path.display()
        ))
    })?;
    held.id = id.to_string();
    Ok(held)
}

fn checked_id(id: &str) -> Result<&str, VivariumError> {
    let sha256 = id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    sha256.then_some(id).ok_or_else(|| {
        VivariumError::Message(format!(
            "'{id}' is not a quarantined letter id (a content sha256); \
             run vivi sync quarantine list"
        ))
    })
}

fn not_held(id: &str) -> VivariumError {
    VivariumError::Message(format!(
        "no quarantined letter {id}; run vivi sync quarantine list"
    ))
}

#[cfg(test)]
#[path = "quarantine_test.rs"]
mod tests;
