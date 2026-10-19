//! Memo tags and filtering: repeated `X-Vivi-Tag` headers, the shared memo
//! filter, `--since` cutoffs, human window labels, and the `[memo]` config
//! table (hidden tags and per-tag retention).

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::duration::parse_duration_secs;
use crate::error::VivariumError;

/// Header that stores one memo tag; a memo carries one header per tag.
const TAG_HEADER: &str = "X-Vivi-Tag";

/// Tags hidden from memo listings unless the caller asks for them.
const DEFAULT_HIDDEN_TAG: &str = "digest";

/// Parse the `X-Vivi-Tag` headers out of a raw eml message.
///
/// Header names match case-insensitively, values are trimmed, empty values
/// are dropped, exact duplicates are removed, and the first-seen order is
/// kept. An unparseable message yields no tags.
#[must_use]
pub fn parse_tags(raw: &[u8]) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    let Some(parsed) = mail_parser::MessageParser::default().parse(raw) else {
        return tags;
    };
    for header in parsed.headers().iter() {
        if !header.name().eq_ignore_ascii_case(TAG_HEADER) {
            continue;
        }
        if let mail_parser::HeaderValue::Text(text) = header.value() {
            let tag = text.trim().to_string();
            if !tag.is_empty() && !tags.contains(&tag) {
                tags.push(tag);
            }
        }
    }
    tags
}

/// The shared memo list/search filter: tag selection, exclusion, a time
/// floor, a result cap, and whether hidden tags stay visible.
#[derive(Debug, Clone, Default)]
pub struct MemoFilter {
    /// Keep only memos carrying every one of these tags (AND).
    pub tags: Vec<String>,
    /// Drop memos carrying any of these tags.
    pub exclude_tags: Vec<String>,
    /// Keep only memos dated at or after this moment.
    pub since: Option<DateTime<Utc>>,
    /// Stop after this many kept memos.
    pub limit: Option<usize>,
    /// Keep memos whose tags are in the hidden set.
    pub include_hidden: bool,
}

/// Whether one memo (view date plus tags) passes `filter` against the
/// `hidden` tag set.
///
/// A memo is kept when no `exclude_tags` member matches (case-insensitive),
/// every `tags` member matches (AND, case-insensitive), `since` is absent or
/// the memo date is at or after it, and its tags avoid the hidden set unless
/// `include_hidden` is set.
#[must_use]
pub fn keep(
    view_date: Option<DateTime<Utc>>,
    tags: &[String],
    filter: &MemoFilter,
    hidden: &BTreeSet<String>,
) -> bool {
    let matches = |tag: &String, wanted: &String| tag.eq_ignore_ascii_case(wanted);
    if filter
        .exclude_tags
        .iter()
        .any(|excluded| tags.iter().any(|tag| matches(tag, excluded)))
    {
        return false;
    }
    if !filter
        .tags
        .iter()
        .all(|wanted| tags.iter().any(|tag| matches(tag, wanted)))
    {
        return false;
    }
    if let Some(since) = filter.since {
        match view_date {
            Some(date) if date >= since => {}
            _ => return false,
        }
    }
    if !filter.include_hidden && tags.iter().any(|tag| hidden_member(hidden, tag)) {
        return false;
    }
    true
}

fn hidden_member(hidden: &BTreeSet<String>, tag: &str) -> bool {
    hidden.iter().any(|name| name.eq_ignore_ascii_case(tag))
}

/// Parse a `--since` argument: a duration counted back from `now` (`24h`),
/// a plain date (`2026-10-01`, midnight UTC), or an RFC3339 timestamp.
///
/// # Errors
/// Returns an error naming the argument when none of the three forms parse.
pub fn since_cutoff(arg: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, VivariumError> {
    if let Ok(secs) = parse_duration_secs(arg) {
        let at = now
            .timestamp()
            .saturating_sub(i64::try_from(secs).unwrap_or(i64::MAX));
        return Utc
            .timestamp_opt(at, 0)
            .single()
            .ok_or_else(|| invalid_since(arg));
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(arg, "%Y-%m-%d") {
        let Some(midnight) = date.and_hms_opt(0, 0, 0) else {
            return Err(invalid_since(arg));
        };
        return Ok(DateTime::<Utc>::from_naive_utc_and_offset(midnight, Utc));
    }
    if let Ok(date) = DateTime::parse_from_rfc3339(arg) {
        return Ok(date.with_timezone(&Utc));
    }
    Err(invalid_since(arg))
}

fn invalid_since(arg: &str) -> VivariumError {
    VivariumError::Message(format!(
        "invalid since value '{arg}'; use a duration like 24h, a date like 2026-10-01, or an RFC3339 timestamp"
    ))
}

/// Render a window length as a human label: whole days, else whole hours,
/// else whole minutes, else raw seconds.
#[must_use]
pub fn window_label(secs: u64) -> String {
    if secs >= 86_400 && secs % 86_400 == 0 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3_600 && secs % 3_600 == 0 {
        format!("{}h", secs / 3_600)
    } else if secs >= 60 && secs % 60 == 0 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// The `[memo]` table of `mailspace.toml`. Absent means the defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoConfig {
    /// Extra tags hidden from memo listings beyond the built-in default.
    #[serde(default)]
    pub hidden_tags: Vec<String>,
    /// Per-tag retention windows, e.g. `retain = { digest = "90d" }`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub retain: BTreeMap<String, String>,
}

/// The hidden tag set for a mailspace: the built-in default plus every
/// configured `hidden_tags` entry.
#[must_use]
pub fn hidden_tags(config: &Option<MemoConfig>) -> BTreeSet<String> {
    let mut hidden = BTreeSet::new();
    hidden.insert(DEFAULT_HIDDEN_TAG.to_string());
    if let Some(config) = config {
        for tag in &config.hidden_tags {
            hidden.insert(tag.clone());
        }
    }
    hidden
}

#[cfg(test)]
#[path = "memos_test.rs"]
mod tests;
