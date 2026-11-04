//! Small display helpers shared by the boot sections.

use chrono::{DateTime, Utc};

use super::goals::cap_chars;

/// Age of a stored timestamp relative to `now`, as `14h` or `3d`.
///
/// An unparseable or missing timestamp reads `?` so a corrupt row never
/// silently reads as fresh.
#[must_use]
pub fn age_label(timestamp: &str, now: DateTime<Utc>) -> String {
    let Ok(at) = DateTime::parse_from_rfc3339(timestamp) else {
        return "?".to_string();
    };
    let seconds = now
        .signed_duration_since(at.with_timezone(&Utc))
        .num_seconds()
        .max(0);
    if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

/// First paragraph of a document, capped to `max_lines` non-empty lines.
///
/// Used for charter heads: enough for a reader to know what a seat is for,
/// without carrying the whole charter.
#[must_use]
pub fn first_paragraph(text: &str, max_lines: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if lines.is_empty() {
                continue;
            }
            break;
        }
        lines.push(cap_chars(trimmed, 120));
        if lines.len() >= max_lines {
            break;
        }
    }
    lines
}

/// Collapse whitespace and cap a single-line display string.
#[must_use]
pub fn one_line(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    cap_chars(&collapsed, max)
}
