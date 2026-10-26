//! The versioned multipart assignment codec: one assignment prose split into
//! encoded part bodies plus a final manifest, all prose-only.
//!
//! A synced mailspace refuses a letter over the 64 KiB letter cap
//! ([`MAX_LETTER_BYTES`](crate::sync::MAX_LETTER_BYTES)), so a long
//! assignment cannot travel as one body. This codec splits the assignment
//! prose at character boundaries into conservative chunks — measured in
//! *encoded* length, so quotes, backslashes and control characters that grow
//! when JSON-escaped still fit — and seals the set with a manifest carrying
//! the job identity, the whole-body SHA-256 and decoded byte length, and one
//! ordered identity per part (index, decoded byte length, SHA-256). Each
//! part body carries the same job identity, its index and the part count,
//! and its own decoded length and hash.
//!
//! A part body and the manifest are each a single-line JSON object, so an
//! encoded body can never open or close a `+++` envelope fence: the
//! convention lives entirely in letter prose and touches no front matter,
//! headers or caps. Encoding refuses a manifest that would not itself fit
//! its budget. Decoding classifies every failure without matching message
//! text: [`VivariumError::JobBriefIncomplete`] when the manifest is intact
//! but part bodies are missing, [`VivariumError::JobBriefCorrupt`] for
//! anything unreadable, inconsistent or tampered.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::VivariumError;

/// The `format` value of an encoded part body.
pub const FORMAT: &str = "vivi-assignment";

/// The `format` value of the final manifest.
pub const MANIFEST_FORMAT: &str = "vivi-assignment-manifest";

/// The codec version; decode refuses any other.
pub const VERSION: u32 = 1;

/// The largest encoded body chunk per part, in bytes. Conservative headroom:
/// the part envelope adds its own fields on top, and the finished letter must
/// still fit the 64 KiB letter cap with room for its headers.
pub const PART_BODY_BUDGET: usize = 16 * 1024;

/// The largest encoded manifest, in bytes. One part costs roughly one
/// identity entry here, so this bounds the part count and with it the
/// largest assignment the codec carries.
pub const MANIFEST_BUDGET: usize = 16 * 1024;

/// An encoded assignment: the part bodies in index order and the final
/// manifest. Send each part body as one letter's prose; send the manifest
/// with the last part or as its own letter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// Encoded part bodies, ordered by part index starting at one.
    pub parts: Vec<String>,
    /// The encoded final manifest.
    pub manifest: String,
}

/// Encode `prose` as the assignment of `job`: part bodies under
/// [`PART_BODY_BUDGET`] and a manifest under [`MANIFEST_BUDGET`].
///
/// # Errors
/// Returns [`VivariumError`] when the manifest that seals the set would not
/// itself fit [`MANIFEST_BUDGET`], which means the job needs a larger part
/// budget to be sendable at all.
pub fn encode(job: &str, prose: &str) -> Result<Assignment, VivariumError> {
    encode_with_budgets(job, prose, PART_BODY_BUDGET, MANIFEST_BUDGET)
}

/// Encode with explicit budgets; `encode` is this with the public constants.
///
/// # Errors
/// Returns [`VivariumError::Message`] when the manifest exceeds
/// `manifest_budget`.
fn encode_with_budgets(
    job: &str,
    prose: &str,
    part_budget: usize,
    manifest_budget: usize,
) -> Result<Assignment, VivariumError> {
    let chunks = split_with_budget(prose, part_budget);
    let parts = chunks
        .iter()
        .enumerate()
        .map(|(at, chunk)| encode_part(job, at + 1, chunks.len(), chunk))
        .collect::<Result<Vec<_>, _>>()?;
    let manifest = encode_manifest(job, prose, &chunks, manifest_budget)?;
    Ok(Assignment { parts, manifest })
}

/// The `body` of one encoded part: format and version, the job identity, the
/// part index and count, and the decoded length and SHA-256 of the chunk.
fn encode_part(
    job: &str,
    index: usize,
    total: usize,
    chunk: &str,
) -> Result<String, VivariumError> {
    let frame = PartBody {
        format: FORMAT.to_owned(),
        version: VERSION,
        job: job.to_owned(),
        part: index as u32,
        parts: total as u32,
        bytes: chunk.len(),
        sha256: sha256_hex(chunk.as_bytes()),
        body: chunk.to_owned(),
    };
    write_json(&frame)
}

/// The body of the final manifest: format and version, the job identity, the
/// part count, the whole decoded length and SHA-256, and one ordered
/// identity per part.
///
/// # Errors
/// Returns [`VivariumError::Message`] when the encoded manifest is over
/// `budget` rather than emit a set that cannot be sent.
fn encode_manifest(
    job: &str,
    prose: &str,
    chunks: &[String],
    budget: usize,
) -> Result<String, VivariumError> {
    let manifest = Manifest {
        format: MANIFEST_FORMAT.to_owned(),
        version: VERSION,
        job: job.to_owned(),
        parts: chunks.len() as u32,
        bytes: prose.len(),
        sha256: sha256_hex(prose.as_bytes()),
        part: chunks
            .iter()
            .enumerate()
            .map(|(at, chunk)| PartIdentity {
                index: at as u32 + 1,
                bytes: chunk.len(),
                sha256: sha256_hex(chunk.as_bytes()),
            })
            .collect(),
    };
    let raw = write_json(&manifest)?;
    if raw.len() > budget {
        return Err(VivariumError::Message(format!(
            "assignment manifest is {} bytes, over the {budget}-byte budget: \
             too many parts to send",
            raw.len()
        )));
    }
    Ok(raw)
}

/// Reassemble the assignment prose from the encoded `manifest` and part
/// bodies, which may arrive in any order. Every manifest rule is checked:
/// version, job identity, ordered part identities, per-part counts, decoded
/// lengths and hashes, and the whole-body decoded length and hash.
///
/// # Errors
/// Returns [`VivariumError::JobBriefIncomplete`] when part bodies named by
/// the manifest are absent, and [`VivariumError::JobBriefCorrupt`] when
/// anything present is unreadable, inconsistent, or fails a hash or length
/// check.
pub fn decode(
    job: &str,
    manifest_raw: &str,
    part_bodies: &[String],
) -> Result<String, VivariumError> {
    let manifest: Manifest = parse_frame(manifest_raw, MANIFEST_FORMAT, "manifest")?;
    if manifest.job != job {
        return Err(corrupt(format!(
            "manifest names job {:?}, expected {job:?}",
            manifest.job
        )));
    }
    if manifest.part.len() != manifest.parts as usize {
        return Err(corrupt(format!(
            "manifest lists {} identities for {} parts",
            manifest.part.len(),
            manifest.parts
        )));
    }
    for (at, identity) in manifest.part.iter().enumerate() {
        if identity.index != at as u32 + 1 {
            return Err(corrupt(format!(
                "manifest identity {} names part {}",
                at + 1,
                identity.index
            )));
        }
    }
    let mut chunks: Vec<Option<String>> = vec![None; manifest.part.len()];
    for raw in part_bodies {
        place_part(job, &manifest, raw, &mut chunks)?;
    }
    assemble(job, &manifest, chunks)
}

/// Validate one part body against the manifest and slot its chunk by index.
///
/// # Errors
/// Returns [`VivariumError::JobBriefCorrupt`] for a malformed, foreign,
/// misnumbered, tampered or duplicate part.
fn place_part(
    job: &str,
    manifest: &Manifest,
    raw: &str,
    chunks: &mut [Option<String>],
) -> Result<(), VivariumError> {
    let part: PartBody = parse_frame(raw, FORMAT, "part")?;
    if part.job != job {
        return Err(corrupt(format!(
            "part {} names job {:?}, expected {job:?}",
            part.part, part.job
        )));
    }
    if part.parts != manifest.parts {
        return Err(corrupt(format!(
            "part {} says {} parts, manifest says {}",
            part.part, part.parts, manifest.parts
        )));
    }
    let at = part.part as usize;
    if at == 0 || at > chunks.len() {
        return Err(corrupt(format!(
            "part {} is outside 1..={}",
            part.part,
            chunks.len()
        )));
    }
    let identity = &manifest.part[at - 1];
    if part.bytes != identity.bytes {
        return Err(corrupt(format!(
            "part {} is {} bytes, manifest says {}",
            part.part, part.bytes, identity.bytes
        )));
    }
    if part.sha256 != identity.sha256 {
        return Err(corrupt(format!(
            "part {} sha256 does not match the manifest identity",
            part.part
        )));
    }
    let digest = sha256_hex(part.body.as_bytes());
    if part.body.len() != part.bytes || digest != part.sha256 {
        return Err(corrupt(format!(
            "part {} body is {} bytes with sha256 {digest}, header says {} with {}",
            part.part,
            part.body.len(),
            part.bytes,
            part.sha256
        )));
    }
    if chunks[at - 1].replace(part.body).is_some() {
        return Err(corrupt(format!("duplicate part {}", part.part)));
    }
    Ok(())
}

/// Join the chunks in order and verify the whole-body length and hash.
///
/// # Errors
/// Returns [`VivariumError::JobBriefIncomplete`] naming the first missing
/// part, and [`VivariumError::JobBriefCorrupt`] when the reassembly fails
/// the manifest's whole-body checks.
fn assemble(
    job: &str,
    manifest: &Manifest,
    chunks: Vec<Option<String>>,
) -> Result<String, VivariumError> {
    let mut whole = String::new();
    for (at, chunk) in chunks.into_iter().enumerate() {
        let Some(chunk) = chunk else {
            return Err(incomplete(format!(
                "job {job:?}: part {} of {} is missing",
                at + 1,
                manifest.parts
            )));
        };
        whole.push_str(&chunk);
    }
    if whole.len() != manifest.bytes {
        return Err(corrupt(format!(
            "job {job:?}: reassembled {} bytes, manifest says {}",
            whole.len(),
            manifest.bytes
        )));
    }
    let digest = sha256_hex(whole.as_bytes());
    if digest != manifest.sha256 {
        return Err(corrupt(format!(
            "job {job:?}: whole sha256 {digest} does not match manifest {}",
            manifest.sha256
        )));
    }
    Ok(whole)
}

/// Split `prose` at character boundaries into chunks whose JSON-escaped
/// length stays within `budget`. Escape growth is per character, so a single
/// character over budget still forms its own chunk rather than being lost.
fn split_with_budget(prose: &str, budget: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut encoded = 0_usize;
    for ch in prose.chars() {
        let cost = encoded_char_len(ch);
        if encoded + cost > budget && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            encoded = 0;
        }
        current.push(ch);
        encoded += cost;
    }
    chunks.push(current);
    chunks
}

/// The bytes `ch` costs in a `serde_json` string literal: two for the short
/// escapes, six for `\u00XX`, and its own UTF-8 length otherwise.
fn encoded_char_len(ch: char) -> usize {
    match ch {
        '"' | '\\' | '\u{8}' | '\t' | '\n' | '\u{c}' | '\r' => 2,
        ch if (ch as u32) < 0x20 => 6,
        ch => ch.len_utf8(),
    }
}

/// Lowercase hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Serialize one frame to the single-line JSON the convention uses.
///
/// # Errors
/// Returns [`VivariumError::Message`] when serialization fails, which for
/// plain string fields cannot.
fn write_json<T: Serialize>(frame: &T) -> Result<String, VivariumError> {
    serde_json::to_string(frame).map_err(|err| VivariumError::Message(format!("encode: {err}")))
}

/// Parse one frame: JSON first, then the format and version head, then the
/// strict typed shape (unknown fields and bad types refuse).
///
/// # Errors
/// Returns [`VivariumError::JobBriefCorrupt`] for any of the three.
fn parse_frame<T: DeserializeOwned>(
    raw: &str,
    format: &str,
    what: &str,
) -> Result<T, VivariumError> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|err| corrupt(format!("{what} is not valid JSON: {err}")))?;
    let got = value.get("format").and_then(Value::as_str);
    if got != Some(format) {
        return Err(corrupt(format!(
            "{what} format {got:?}, expected {format:?}"
        )));
    }
    let version = value.get("version").and_then(Value::as_u64);
    if version != Some(u64::from(VERSION)) {
        return Err(corrupt(format!(
            "{what} version {version:?}, expected {VERSION}"
        )));
    }
    serde_json::from_value(value).map_err(|err| corrupt(format!("{what} is malformed: {err}")))
}

/// A [`VivariumError::JobBriefIncomplete`] with its reason.
fn incomplete(reason: String) -> VivariumError {
    VivariumError::JobBriefIncomplete(reason)
}

/// A [`VivariumError::JobBriefCorrupt`] with its reason.
fn corrupt(reason: String) -> VivariumError {
    VivariumError::JobBriefCorrupt(reason)
}

/// One encoded part body.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartBody {
    format: String,
    version: u32,
    job: String,
    part: u32,
    parts: u32,
    bytes: usize,
    sha256: String,
    body: String,
}

/// The final manifest.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    version: u32,
    job: String,
    parts: u32,
    bytes: usize,
    sha256: String,
    part: Vec<PartIdentity>,
}

/// One ordered part identity in the manifest.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartIdentity {
    index: u32,
    bytes: usize,
    sha256: String,
}

#[cfg(test)]
#[path = "job_brief_test.rs"]
mod tests;
