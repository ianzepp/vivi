//! Memo tags and filtering: letter front-matter tags, the shared memo
//! filter, `--since` cutoffs, human window labels, and the `[memo]` config
//! table (hidden tags and per-tag retention).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::duration::parse_duration_secs;
use crate::error::VivariumError;
use crate::letter::{Front, of_message};
use crate::mailspace::Mailspace;
use crate::storage::StoredMessageView;

/// Tags hidden from memo listings unless the caller asks for them.
const DEFAULT_HIDDEN_TAG: &str = "digest";

/// The tags of a stored memo: the front matter of its format-1 letter. A
/// legacy memo (tag header, no format header) is history: its header tag is
/// ignored. An unparseable message yields no tags.
#[must_use]
pub fn parse_tags(raw: &[u8]) -> Vec<String> {
    match of_message(raw, "") {
        Ok(letter) => match letter.front {
            Front::Memo(front) => front.tags,
            _ => Vec::new(),
        },
        Err(_) => Vec::new(),
    }
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

/// The configured retention window for `tag`, in seconds. A missing rule or
/// an invalid duration means that matching memos are retained indefinitely.
#[must_use]
pub fn retain_secs(config: &Option<MemoConfig>, tag: &str) -> Option<u64> {
    let config = config.as_ref()?;
    config
        .retain
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(tag.trim()))
        .and_then(|(_, value)| match parse_duration_secs(value) {
            Ok(secs) => Some(secs),
            Err(_) => None,
        })
}

/// Return handles of memos tagged `tag` whose RFC3339 dates are strictly
/// older than the retention cutoff. Tags match case-insensitively; bad dates
/// are retained.
#[must_use]
pub fn expired_handles(
    memos: &[(StoredMessageView, Vec<String>)],
    tag: &str,
    retain_secs: u64,
    now: DateTime<Utc>,
) -> Vec<String> {
    let seconds = i64::try_from(retain_secs).unwrap_or(i64::MAX);
    let cutoff = now
        .checked_sub_signed(chrono::Duration::seconds(seconds))
        .unwrap_or(DateTime::<Utc>::MIN_UTC);
    memos
        .iter()
        .filter(|(_, tags)| {
            tags.iter()
                .any(|seen| seen.eq_ignore_ascii_case(tag.trim()))
        })
        .filter(|(view, _)| {
            DateTime::parse_from_rfc3339(view.date.trim())
                .is_ok_and(|date| date.with_timezone(&Utc) < cutoff)
        })
        .map(|(view, _)| view.handle.clone())
        .collect()
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

/// Parse a stored message date (RFC3339) into UTC, or `None` when it does
/// not parse.
fn memo_date(raw: &str) -> Option<DateTime<Utc>> {
    match DateTime::parse_from_rfc3339(raw) {
        Ok(date) => Some(date.with_timezone(&Utc)),
        Err(_) => None,
    }
}

/// Sort newest-first (undated last, stable) and apply the filter's result cap.
fn sort_and_limit(
    mut memos: Vec<(StoredMessageView, Vec<String>)>,
    filter: &MemoFilter,
) -> Vec<(StoredMessageView, Vec<String>)> {
    memos.sort_by(
        |left, right| match (memo_date(&left.0.date), memo_date(&right.0.date)) {
            (Some(left), Some(right)) => right.cmp(&left),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
    );
    if let Some(limit) = filter.limit {
        memos.truncate(limit);
    }
    memos
}

impl Mailspace {
    /// List memos for `identity` with `filter` applied, newest first.
    ///
    /// Each memo comes back with its parsed front-matter tags. Hidden tags
    /// (the built-in `digest` default plus the configured `hidden_tags`) are
    /// dropped unless `filter.include_hidden` is set.
    ///
    /// # Errors
    /// Returns an error if the identity cannot be resolved or a storage
    /// operation fails.
    pub fn list_memos_filtered(
        &self,
        identity: &str,
        filter: &MemoFilter,
    ) -> Result<Vec<(StoredMessageView, Vec<String>)>, VivariumError> {
        let hidden = hidden_tags(&self.config.memo);
        let storage = self.storage()?;
        let mut kept = Vec::new();
        for view in self.list_kind(Some(identity), "memos", "memo")? {
            let data = storage.read_message(&view.message_id)?;
            let tags = parse_tags(&data);
            if keep(memo_date(&view.date), &tags, filter, &hidden) {
                kept.push((view, tags));
            }
        }
        Ok(sort_and_limit(kept, filter))
    }

    /// [`Mailspace::search_memos`] with `filter` applied and tags attached.
    ///
    /// Matching keeps the same rules as [`Mailspace::search_memos`]: subject
    /// and body (unless `subject_only`), case-insensitive. Every match is
    /// then filtered, tagged, sorted newest first, and capped at
    /// `filter.limit`.
    ///
    /// # Errors
    /// Returns an error if the identity cannot be resolved, a storage
    /// operation fails, or a memo blob cannot be parsed.
    pub fn search_memos_filtered(
        &self,
        identity: &str,
        query: &str,
        subject_only: bool,
        filter: &MemoFilter,
    ) -> Result<Vec<(StoredMessageView, Vec<String>)>, VivariumError> {
        let hidden = hidden_tags(&self.config.memo);
        let query = query.to_lowercase();
        let storage = self.storage()?;
        let mut matched = Vec::new();
        for memo in self.list_kind(Some(identity), "memos", "memo")? {
            let data = storage.read_message(&memo.message_id)?;
            let tags = parse_tags(&data);
            if !keep(memo_date(&memo.date), &tags, filter, &hidden) {
                continue;
            }
            let hit = memo.subject.to_lowercase().contains(&query)
                || (!subject_only
                    && crate::extract::extract_text(&data)?
                        .to_lowercase()
                        .contains(&query));
            if hit {
                matched.push((memo, tags));
            }
        }
        Ok(sort_and_limit(matched, filter))
    }
}

#[cfg(test)]
#[path = "memos_test.rs"]
mod tests;
