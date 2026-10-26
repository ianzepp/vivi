//! Operator-only task evidence for one task: bounded thread messages and
//! newest-first attempt rows, built from the complete connected thread.
//!
//! Pure diagnostics: no store, no clock, no writes. The forthcoming
//! `task_thread` endpoint locates the task and hands its thread messages
//! here; this module orders them with the mailspace's own note order
//! ([`crate::mailspace::order_notes`]), classifies attempt notes with the
//! existing parsers ([`crate::mailspace::pump_note::note_of`] and
//! [`crate::pump::attempt_read`]), and bounds every body, keeping the
//! failure evidence (outcome, failing command, excerpt tail) ahead of the
//! optional verbose final-message prose.

use crate::mailspace::MailspaceThreadMessage;
use crate::mailspace::{order_notes, pump_note};
use crate::pump::attempt_read::{self, AttemptFields};

/// Most bytes of an ordinary thread message's body.
const BODY_CAP: usize = 4 * 1024;
/// Most bytes of an attempt row's raw body when it has no record sections.
const ATTEMPT_BODY_CAP: usize = 4 * 1024;
/// Most bytes of the failure excerpt tail an attempt row keeps.
const EXCERPT_TAIL: usize = 1024;
/// Most bytes of the final-message tail an attempt row keeps.
const MESSAGE_TAIL: usize = 512;

/// One page cut from a full list: the offset and limit the caller named.
#[derive(Debug, Clone, Copy)]
pub(super) struct Window {
    /// First row of the page in the full list.
    pub(super) offset: usize,
    /// Most rows the page may hold.
    pub(super) limit: usize,
}

/// One bounded thread message, in thread order.
#[derive(Debug, Clone)]
pub(super) struct MessageRow {
    pub(super) handle: String,
    pub(super) message_id: String,
    pub(super) date: String,
    pub(super) from: String,
    pub(super) subject: String,
    /// The lifecycle event the letter records, when it is an event letter.
    pub(super) event: Option<String>,
    /// The body, capped at [`BODY_CAP`] bytes from its head.
    pub(super) body: String,
    /// Whether [`BODY_CAP`] cut the body.
    pub(super) body_truncated: bool,
    /// Whether the message is a valid attempt note.
    pub(super) is_attempt: bool,
}

/// One attempt: the message it came from, its parsed metadata, and a bounded
/// body that keeps the failure evidence.
#[derive(Debug, Clone)]
pub(super) struct AttemptRow {
    pub(super) handle: String,
    pub(super) message_id: String,
    pub(super) date: String,
    /// The record block or receipt the message carries.
    pub(super) fields: AttemptFields,
    /// The outcome, failing command and excerpt tail, then the final-message
    /// tail — or the raw body capped at [`ATTEMPT_BODY_CAP`].
    pub(super) body: String,
    /// Whether any part of the body was left out.
    pub(super) body_truncated: bool,
}

/// The evidence one task's thread carries: paged messages and independently
/// paged newest-first attempts over the same complete thread.
#[derive(Debug)]
pub(super) struct TaskEvidence {
    pub(super) messages: Vec<MessageRow>,
    pub(super) total: usize,
    pub(super) offset: usize,
    pub(super) limit: usize,
    /// The next page's offset, when more messages follow; an empty page at
    /// or past the end is the terminal page.
    pub(super) next_offset: Option<usize>,
    pub(super) truncated: bool,
    pub(super) attempts: Vec<AttemptRow>,
    pub(super) attempts_total: usize,
    pub(super) attempts_offset: usize,
    pub(super) attempts_limit: usize,
    pub(super) attempts_next_offset: Option<usize>,
    pub(super) attempts_truncated: bool,
}

/// The evidence one task's complete connected thread carries: its messages
/// in mailspace note order (oldest first), paged by `messages`, and its
/// attempt notes newest-first, paged by `attempts`. Attempts are picked and
/// ordered over the whole thread before paging, so older replies cannot
/// hide a recent failure, and the two pages cut independently.
#[must_use]
pub(super) fn task_evidence(
    thread: &[MailspaceThreadMessage],
    messages: Window,
    attempts: Window,
) -> TaskEvidence {
    let mut ordered = thread.to_vec();
    order_notes(&mut ordered);
    let mut rows = Vec::with_capacity(ordered.len());
    let mut attempt_rows = Vec::new();
    for message in &ordered {
        let fields = attempt_fields(&message.body);
        let is_attempt = fields.is_some();
        if let Some(fields) = fields {
            attempt_rows.push(attempt_row(message, fields));
        }
        rows.push(message_row(message, is_attempt));
    }
    let total = rows.len();
    attempt_rows.reverse();
    let attempts_total = attempt_rows.len();
    let picked = pick(rows, total, messages);
    let picked_attempts = pick(attempt_rows, attempts_total, attempts);
    TaskEvidence {
        total: picked.total,
        messages: picked.rows,
        offset: picked.offset,
        limit: picked.limit,
        next_offset: picked.next_offset,
        truncated: picked.truncated,
        attempts_total: picked_attempts.total,
        attempts: picked_attempts.rows,
        attempts_offset: picked_attempts.offset,
        attempts_limit: picked_attempts.limit,
        attempts_next_offset: picked_attempts.next_offset,
        attempts_truncated: picked_attempts.truncated,
    }
}

/// One paged list: the rows kept plus the receipt for the cut.
struct Picked<T> {
    rows: Vec<T>,
    total: usize,
    offset: usize,
    limit: usize,
    next_offset: Option<usize>,
    truncated: bool,
}

/// Cut `rows` (of `total`) to `window`, keeping the receipt with them.
fn pick<T>(rows: Vec<T>, total: usize, window: Window) -> Picked<T> {
    let (start, end, next_offset, truncated) = cut(total, window);
    Picked {
        rows: rows.into_iter().skip(start).take(end - start).collect(),
        total,
        offset: window.offset,
        limit: window.limit,
        next_offset,
        truncated,
    }
}

/// The rows `window` selects over `total` rows: `(start, end)` bounds,
/// `next_offset` when rows follow, and whether any row was left out. An
/// offset at or past the end is the empty terminal page.
fn cut(total: usize, window: Window) -> (usize, usize, Option<usize>, bool) {
    if window.offset >= total {
        return (total, total, None, false);
    }
    let end = window.offset.saturating_add(window.limit).min(total);
    let truncated = end < total;
    (window.offset, end, truncated.then_some(end), truncated)
}

/// The attempt metadata `body` carries: an attempt record — a pump note's
/// prose or a bare reply — by [`attempt_read::parse_record`], else a pool
/// result receipt by [`attempt_read::parse_receipt`]. `None` leaves the
/// message ordinary content, however note-like it looks.
fn attempt_fields(body: &str) -> Option<AttemptFields> {
    let note = pump_note::note_of(body);
    let prose = note.as_ref().map_or(body, |note| note.prose.as_str());
    attempt_read::parse_record(prose).or_else(|| attempt_read::parse_receipt(prose))
}

/// The bounded ordinary-content row for one thread message: its head kept,
/// its truncation exposed.
fn message_row(message: &MailspaceThreadMessage, is_attempt: bool) -> MessageRow {
    let (body, body_truncated) = head_cut(&message.body, BODY_CAP);
    MessageRow {
        handle: message.handle.clone(),
        message_id: message.message_id.clone(),
        date: message.date.clone(),
        from: message.from.clone(),
        subject: message.subject.clone(),
        event: message.event.clone(),
        body,
        body_truncated,
        is_attempt,
    }
}

/// The attempt row for one thread message with its parsed `fields`.
fn attempt_row(message: &MailspaceThreadMessage, fields: AttemptFields) -> AttemptRow {
    let (body, body_truncated) = attempt_body(&message.body, &fields);
    AttemptRow {
        handle: message.handle.clone(),
        message_id: message.message_id.clone(),
        date: message.date.clone(),
        fields,
        body,
        body_truncated,
    }
}

/// The bounded body of one attempt row. A record body composes to the
/// outcome, the failing command, then the excerpt tail and the final-message
/// tail, so the failure evidence survives verbose prose; a body with no
/// record sections (a pool receipt) keeps its raw head.
fn attempt_body(body: &str, fields: &AttemptFields) -> (String, bool) {
    let parts = sections(body);
    if parts.is_empty() {
        return head_cut(body, ATTEMPT_BODY_CAP);
    }
    let mut out = format!("outcome: {}\n", fields.outcome);
    let mut truncated = false;
    if let Some(failing) = parts.failing {
        out.push_str("failing command: ");
        out.push_str(failing.trim());
        out.push('\n');
    }
    for (heading, text, cap) in [
        ("excerpt tail", parts.excerpt, EXCERPT_TAIL),
        ("message tail", parts.message, MESSAGE_TAIL),
    ] {
        let Some(text) = text else { continue };
        let (tail, cut) = tail_lines(&text, cap);
        truncated |= cut;
        out.push_str(heading);
        out.push_str(":\n");
        out.push_str(&tail);
    }
    (out, truncated)
}

/// The record's indented sections: `failing command`, `excerpt`, and the
/// `hand's final message`, each the de-indented lines under its heading
/// until the next heading or an unindented line.
#[derive(Debug, Default)]
struct Sections {
    failing: Option<String>,
    excerpt: Option<String>,
    message: Option<String>,
}

impl Sections {
    /// Whether no section was found: the body is not a record.
    fn is_empty(&self) -> bool {
        self.failing.is_none() && self.excerpt.is_none() && self.message.is_none()
    }
}

/// One record section, named by its heading line.
#[derive(Debug, Clone, Copy)]
enum Section {
    Failing,
    Excerpt,
    Message,
}

/// The section `line` opens, when it is a record heading.
fn heading_of(line: &str) -> Option<Section> {
    match line.trim_end() {
        "failing command:" => Some(Section::Failing),
        "excerpt:" => Some(Section::Excerpt),
        "hand's final message:" => Some(Section::Message),
        _ => None,
    }
}

/// The record sections of an attempt-record body.
fn sections(body: &str) -> Sections {
    let mut out = Sections::default();
    let mut current: Option<Section> = None;
    for line in body.lines() {
        if let Some(opened) = heading_of(line) {
            current = Some(opened);
            continue;
        }
        let Some(section) = current else {
            continue;
        };
        let Some(indented) = line.strip_prefix("  ") else {
            if !line.trim().is_empty() {
                current = None;
            }
            continue;
        };
        let slot = match section {
            Section::Failing => &mut out.failing,
            Section::Excerpt => &mut out.excerpt,
            Section::Message => &mut out.message,
        };
        push_line(slot, indented);
    }
    out
}

/// Append `line` to `slot`, keeping one newline between a section's lines.
fn push_line(slot: &mut Option<String>, line: &str) {
    let text = slot.get_or_insert_with(String::new);
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(line);
}

/// The first `cap` bytes of `text`, cut back to a char boundary, with
/// whether the cap cut anything.
fn head_cut(text: &str, cap: usize) -> (String, bool) {
    if text.len() <= cap {
        return (text.to_owned(), false);
    }
    let mut cut = cap;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    (text[..cut].to_owned(), true)
}

/// The last whole lines of `text` within `cap` bytes, dropping whole lines
/// from the front, with whether anything was dropped. One line longer than
/// `cap` keeps its own tail.
fn tail_lines(text: &str, cap: usize) -> (String, bool) {
    if text.len() <= cap {
        return (format!("{text}\n"), false);
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0usize;
    for line in text.lines().rev() {
        let len = line.len() + 1;
        if used + len > cap {
            break;
        }
        used += len;
        kept.push(line);
    }
    if kept.is_empty() {
        let last = text.lines().next_back().unwrap_or("");
        return (tail_bytes(last, cap), true);
    }
    kept.reverse();
    (format!("{}\n", kept.join("\n")), true)
}

/// The last `cap` bytes of `text`, cut forward to a char boundary.
fn tail_bytes(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_owned();
    }
    let mut cut = text.len().saturating_sub(cap);
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    text[cut..].to_owned()
}

#[cfg(test)]
#[path = "task_evidence_test.rs"]
mod tests;
