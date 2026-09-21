//! Goal document reading: Status line, register tally, and frontier.
//!
//! A registered goal is a path Vivi stores; the content stays in the file and
//! its shape is a project convention. Boot therefore reads only what every
//! factory document has in common:
//!
//! - a machine-parseable `**Status**:` line, whose leading word is the audit
//!   bucket;
//! - markdown tables that carry a `Status` column — the per-item register that
//!   is the completion contract.
//!
//! From those two it derives a tally, a Status-line-versus-register count
//! check, and the pending frontier. No project-specific vocabulary is encoded.

use serde::Serialize;

use super::truncate::{CAP_FRONTIER, Truncation};

/// Leading phrases that read as one status rather than two words.
const MULTI_WORD_STATUSES: [&str; 3] = ["in flight", "in progress", "not started"];

/// Status slugs that count as remaining work.
const FRONTIER_STATUSES: [&str; 3] = ["pending", "in flight", "in progress"];

/// Status slug that satisfies a Status-line completion claim.
pub const DONE_STATUS: &str = "done";

/// Everything boot reads from one registered goal document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GoalRead {
    /// Audit bucket: the leading word of the Status line.
    pub bucket: Option<String>,
    /// Full Status line text, capped for display.
    pub status_text: Option<String>,
    /// Register rows and their status tally.
    pub register: Option<RegisterTally>,
    /// `N/M` completion claim parsed from the Status line.
    pub claim: Option<(usize, usize)>,
    /// True when the claim's numerator disagrees with the register's done count.
    pub claim_mismatch: bool,
    /// Register rows still carrying a remaining-work status.
    pub frontier: Vec<FrontierUnit>,
    /// Caps that bit while reading this document.
    pub truncations: Vec<Truncation>,
}

/// Status-column tally for one register table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegisterTally {
    pub rows: usize,
    /// Status slug and count, ordered by descending count then slug.
    pub counts: Vec<(String, usize)>,
}

impl RegisterTally {
    /// Count of rows carrying `slug`.
    #[must_use]
    pub fn count_of(&self, slug: &str) -> usize {
        self.counts
            .iter()
            .filter(|(name, _)| name == slug)
            .map(|(_, count)| count)
            .sum()
    }
}

/// One register row that is not yet done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrontierUnit {
    pub unit: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Read a goal document body.
#[must_use]
pub fn read_goal(text: &str) -> GoalRead {
    let status = status_line(text);
    let tables = parse_tables(text);
    let register = status_table(&tables).map(|table| tally(&table.rows, table.status_column));
    let claim = status.as_deref().and_then(completion_claim);
    let mut frontier = frontier_units(&tables);
    let truncations = cap_frontier(&mut frontier);
    let claim_mismatch = match (&claim, &register) {
        (Some((claimed, _)), Some(tally)) => *claimed != tally.count_of(DONE_STATUS),
        _ => false,
    };
    GoalRead {
        bucket: status.as_deref().map(bucket),
        status_text: status,
        register,
        claim,
        claim_mismatch,
        frontier,
        truncations,
    }
}

fn status_line(text: &str) -> Option<String> {
    text.lines()
        .map(|line| line.trim_start_matches(['>', ' ', '\t']))
        .find_map(|line| {
            line.strip_prefix("**Status**:")
                .map(|rest| rest.trim().to_string())
        })
        .filter(|rest| !rest.is_empty())
}

/// The audit bucket is the leading word, so a status that opens "partially
/// delivered" and one that opens "planned" land in different buckets.
#[must_use]
pub fn bucket(status_text: &str) -> String {
    status_text
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Parse an `N/M` completion claim from a Status line.
#[must_use]
pub fn completion_claim(status_text: &str) -> Option<(usize, usize)> {
    let chars: Vec<char> = status_text.chars().collect();
    for (index, ch) in chars.iter().enumerate() {
        if *ch != '/' {
            continue;
        }
        // The numerator sits immediately before the '/'. Walk back to the start
        // of its digit run, then read it forwards: collecting the run backwards
        // and handing it to digits_to_usize transposes multi-digit values, so
        // `24/26` parsed as 42 — and a single-digit fixture cannot catch that.
        let left_start = chars[..index]
            .iter()
            .rposition(|c| !c.is_ascii_digit())
            .map_or(0, |i| i + 1);
        let left: Vec<char> = chars[left_start..index].to_vec();
        let right: Vec<char> = chars[index + 1..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .copied()
            .collect();
        if let (Some(left), Some(right)) = (digits_to_usize(&left), digits_to_usize(&right)) {
            return Some((left, right));
        }
    }
    None
}

/// Read a run of digit characters, rejecting an empty run or an overflow.
fn digits_to_usize(digits: &[char]) -> Option<usize> {
    if digits.is_empty() {
        return None;
    }
    let mut value: usize = 0;
    for ch in digits {
        value = value
            .checked_mul(10)?
            .checked_add(ch.to_digit(10)? as usize)?;
    }
    Some(value)
}

/// One markdown table: its header cells and its data rows.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Table {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    status_column: usize,
}

fn parse_tables(text: &str) -> Vec<Table> {
    let mut tables = Vec::new();
    let mut block: Vec<Vec<String>> = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with('|') {
            block.push(split_cells(line));
            continue;
        }
        flush_table(&mut block, &mut tables);
    }
    flush_table(&mut block, &mut tables);
    tables
}

fn flush_table(block: &mut Vec<Vec<String>>, tables: &mut Vec<Table>) {
    let lines = std::mem::take(block);
    let mut iter = lines.into_iter();
    let Some(header) = iter.next() else {
        return;
    };
    let Some(status_column) = status_column(&header) else {
        return;
    };
    let rows: Vec<Vec<String>> = iter.filter(|row| !is_separator(row)).collect();
    if rows.is_empty() {
        return;
    }
    tables.push(Table {
        header,
        rows,
        status_column,
    });
}

fn status_column(header: &[String]) -> Option<usize> {
    header
        .iter()
        .position(|cell| plain(cell).eq_ignore_ascii_case("status"))
}

fn is_separator(row: &[String]) -> bool {
    !row.is_empty()
        && row.iter().all(|cell| {
            let trimmed = cell.trim();
            !trimmed.is_empty() && trimmed.chars().all(|c| c == '-' || c == ':')
        })
}

/// Split a markdown row on unescaped pipes, dropping the outer edges.
fn split_cells(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for ch in line.trim().chars() {
        match ch {
            '\\' if !escaped => escaped = true,
            '|' if !escaped => {
                cells.push(current.trim().to_string());
                current.clear();
            }
            _ => {
                escaped = false;
                current.push(ch);
            }
        }
    }
    cells.push(current.trim().to_string());
    if cells.first().is_some_and(String::is_empty) {
        cells.remove(0);
    }
    if cells.last().is_some_and(String::is_empty) {
        cells.pop();
    }
    cells
}

/// Strip markdown emphasis and code ticks from a cell.
fn plain(cell: &str) -> String {
    cell.replace(['*', '`'], "").trim().to_string()
}

/// Normalize a status cell to a single slug.
#[must_use]
pub fn normalize_status(cell: &str) -> String {
    let text = plain(cell).to_lowercase().replace('-', " ");
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for phrase in MULTI_WORD_STATUSES {
        if collapsed.starts_with(phrase) {
            return phrase.to_string();
        }
    }
    collapsed
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_string()
}

fn status_table(tables: &[Table]) -> Option<&Table> {
    tables.iter().max_by_key(|table| table.rows.len())
}

fn tally(rows: &[Vec<String>], column: usize) -> RegisterTally {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for row in rows {
        let Some(cell) = row.get(column) else {
            continue;
        };
        let slug = normalize_status(cell);
        match counts.iter_mut().find(|(name, _)| *name == slug) {
            Some((_, count)) => *count += 1,
            None => counts.push((slug, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    RegisterTally {
        rows: rows.len(),
        counts,
    }
}

fn frontier_units(tables: &[Table]) -> Vec<FrontierUnit> {
    let Some(table) = status_table(tables) else {
        return Vec::new();
    };
    table
        .rows
        .iter()
        .filter_map(|row| frontier_unit(table, row))
        .collect()
}

fn frontier_unit(table: &Table, row: &[String]) -> Option<FrontierUnit> {
    let status = normalize_status(row.get(table.status_column)?);
    if !FRONTIER_STATUSES.contains(&status.as_str()) {
        return None;
    }
    let unit = plain(row.first()?);
    if unit.is_empty() {
        return None;
    }
    let note = edge_note(table, row);
    Some(FrontierUnit { unit, status, note })
}

/// The last cell of a wide register reads as the row's note in the factory
/// template. Narrower tables carry no note.
fn edge_note(table: &Table, row: &[String]) -> Option<String> {
    if table.header.len() < 4 || row.len() < 4 {
        return None;
    }
    let note = plain(row.last()?);
    if note.is_empty() || note == "—" || note == "-" {
        return None;
    }
    Some(cap_chars(&note, 64))
}

fn cap_frontier(frontier: &mut Vec<FrontierUnit>) -> Vec<Truncation> {
    let total = frontier.len();
    if total <= CAP_FRONTIER {
        return Vec::new();
    }
    frontier.truncate(CAP_FRONTIER);
    Truncation::of("goal frontier", CAP_FRONTIER, total)
        .into_iter()
        .collect()
}

/// Truncate to `max` characters on a character boundary.
#[must_use]
pub fn cap_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}
