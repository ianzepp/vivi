//! Bounded, action-aware rendering of one tool answer into a receipt.
//!
//! Rendering is data preparation, not trust: the prompt boundary fences
//! and defuses whatever a receipt carries. A diagnostic read's receipt
//! keeps the evidence a later turn plans from — the canonical anchor, the
//! item's state and event, the page facts, every available handle — ahead
//! of any long body, and visibly marks what it omits and how to read on.

use std::fmt::Write as _;

use serde_json::Value;

use super::ACTION_RESULT_ALLOWANCE;
use crate::letter::records::NoteWord;
use crate::mailspace::pump_note::note_of;
use crate::mind::authority::Action;

/// Longest text kept in an ordinary (nondiagnostic) receipt or a failure.
const MAX_RECEIPT: usize = 1500;

/// Characters reserved for an omission mark, so a cut body's marker never
/// pushes a receipt past its budget.
const MARK_RESERVE: usize = 80;

/// The marker a hard-cut text ends with.
const CUT_MARK: &str = "\n[cut]";

/// Whether the action's answers are diagnostic reads a later turn plans
/// from, rather than receipts of writes and control moves.
fn is_diagnostic(kind: Action) -> bool {
    matches!(
        kind,
        Action::ReadStatus
            | Action::ReadLog
            | Action::ReadNeed
            | Action::ReadNeedThread
            | Action::ReadTaskThread
            | Action::ReadFlow
            | Action::ReadConfig
            | Action::ReadPool
    )
}

/// The receipt for one successful call: the tool's name, then the answer
/// rendered for the action's kind. A diagnostic read is parsed and
/// structured within the shared allowance; anything else stays bounded
/// text, so ordinary receipts never grow.
pub(super) fn receipt(kind: Action, tool: &str, answer: &str) -> String {
    if !is_diagnostic(kind) {
        return format!("{tool}: {}", bounded(answer, MAX_RECEIPT));
    }
    let room = budget(tool);
    let body = match serde_json::from_str::<Value>(answer) {
        Ok(parsed) => diagnostic(kind, &parsed, room),
        Err(_) => bounded(answer, room),
    };
    format!("{tool}: {body}")
}

/// The failure text: the tool's name and the bounded error, so a huge
/// refusal cannot flood a turn either.
pub(super) fn failure(tool: &str, error: &str) -> String {
    format!("{tool}: {}", bounded(error, MAX_RECEIPT))
}

/// The structured render of one parsed diagnostic answer.
fn diagnostic(kind: Action, answer: &Value, room: usize) -> String {
    match kind {
        Action::ReadTaskThread => thread(answer, room, true),
        Action::ReadNeedThread => thread(answer, room, false),
        Action::ReadNeed => need_show(answer, room),
        _ => bounded(&answer.to_string(), room),
    }
}

/// Characters one receipt body may spend: the shared allowance less the
/// `tool: ` prefix.
fn budget(tool: &str) -> usize {
    ACTION_RESULT_ALLOWANCE.saturating_sub(tool.chars().count() + 2)
}

/// One thread answer, structured: the page facts first, then on a task
/// thread the attempt records' evidence before the long root and hand
/// messages, then every remaining message's handle with a bounded body.
/// Ordinary replies and job summaries are never presented as attempts.
fn thread(answer: &Value, room: usize, attempts_first: bool) -> String {
    let mut out = thread_header(answer);
    let mut left = room.saturating_sub(out.chars().count());
    let empty: Vec<Value> = Vec::new();
    let messages = answer
        .get("messages")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let (attempts, others): (Vec<&Value>, Vec<&Value>) = if attempts_first {
        messages.iter().partition(|message| is_attempt(message))
    } else {
        (Vec::new(), messages.iter().collect())
    };
    let done_attempts = render_each(&mut out, &mut left, &attempts, true);
    let done_others = render_each(&mut out, &mut left, &others, false);
    let mut skipped: Vec<&Value> = Vec::new();
    skipped.extend(attempts.iter().skip(done_attempts));
    skipped.extend(others.iter().skip(done_others));
    if !skipped.is_empty() {
        let handles: Vec<&str> = skipped.iter().filter_map(|m| field(m, "handle")).collect();
        let _ignored = writeln!(
            out,
            "[{} messages omitted: handles {}]",
            skipped.len(),
            handles.join(", ")
        );
    }
    bounded(&out, room)
}

/// Append `messages` while the budget lasts: each keeps its identity
/// line, and its body gets an equal share of what is left. Returns how
/// many were rendered.
fn render_each(out: &mut String, left: &mut usize, messages: &[&Value], attempts: bool) -> usize {
    let mut rendered = 0;
    while rendered < messages.len() {
        if *left == 0 {
            break;
        }
        let share = *left / (messages.len() - rendered);
        let message = messages[rendered];
        let text = if attempts {
            attempt_text(message, share)
        } else {
            message_text(message, share)
        };
        *left = left.saturating_sub(text.chars().count());
        out.push_str(&text);
        rendered += 1;
    }
    rendered
}

/// The page facts of a thread answer, before any body: the canonical
/// anchor, the item's state and newest event, the page counts and the
/// continuation cursor.
fn thread_header(answer: &Value) -> String {
    let mut header = String::new();
    let _ignored = writeln!(
        header,
        "handle: {}",
        field(answer, "handle").unwrap_or("none")
    );
    let _ignored = writeln!(
        header,
        "state: {}",
        field(answer, "state").unwrap_or("none")
    );
    header.push_str(&event_line(answer));
    let total = answer.get("total").and_then(Value::as_u64).unwrap_or(0);
    let page = answer
        .get("messages")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let truncated = answer
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let _ignored = writeln!(
        header,
        "messages: {} of {} shown, {}",
        page,
        total,
        if truncated {
            "older letters exist"
        } else {
            "complete"
        }
    );
    if let Some(next) = field(answer, "next_before") {
        let _ignored = writeln!(
            header,
            "next_before: {next} (read older letters with before={next})"
        );
    }
    header
}

/// The item's event line: its state, newest event and heads, `none` when
/// the answer carries no event.
fn event_line(answer: &Value) -> String {
    let Some(event) = answer.get("event").filter(|event| !event.is_null()) else {
        return String::from("event: none\n");
    };
    format!(
        "event: state={} event={} heads={}\n",
        field(event, "state").unwrap_or("none"),
        field(event, "event").unwrap_or("none"),
        event
            .get("heads")
            .map_or_else(|| String::from("none"), |heads| heads.to_string()),
    )
}

/// One non-attempt message: its identity line always, its body within
/// the message's share, visibly cut or omitted when it does not fit.
fn message_text(message: &Value, share: usize) -> String {
    let body = message
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let head = format!(
        "{} {} from={} subject: {}\n",
        field(message, "handle").unwrap_or("none"),
        field(message, "kind").unwrap_or("mail"),
        field(message, "role").unwrap_or("none"),
        field(message, "subject").unwrap_or("").replace('\n', " "),
    );
    let room = share.saturating_sub(head.chars().count());
    format!("{head}{}\n", body_within(body, room))
}

/// One attempt record: its handle and the outcome, failing command and
/// excerpt the record carries, ahead of its bounded prose. Built only
/// from a `pump.note = "attempt"` letter; an ordinary reply or a job
/// summary never lands here.
fn attempt_text(message: &Value, share: usize) -> String {
    let handle = field(message, "handle").unwrap_or("none");
    let body = message.get("body").and_then(Value::as_str).unwrap_or("");
    let prose = note_of(body).map_or_else(String::new, |note| note.prose);
    let lines: Vec<&str> = prose.lines().collect();
    let mut out = format!("attempt {handle} outcome: {}\n", outcome_of(&lines));
    if let Some(command) = line_after(&lines, "failing command:") {
        let _ignored = writeln!(out, "failing command: {}", one_line(command));
    }
    if let Some(excerpt) = line_after(&lines, "excerpt:") {
        let _ignored = writeln!(out, "excerpt: {}", one_line(excerpt));
    }
    let room = share.saturating_sub(out.chars().count());
    let _ignored = writeln!(out, "{}", body_within(prose.trim(), room));
    out
}

/// Whether one thread message is an actual attempt record: a letter whose
/// `[pump]` table is marked `attempt`. Ordinary replies and job summaries
/// are never attempts, whatever their prose claims.
fn is_attempt(message: &Value) -> bool {
    message
        .get("body")
        .and_then(Value::as_str)
        .and_then(note_of)
        .is_some_and(|note| note.pump.note == NoteWord::Attempt)
}

/// The `outcome:` value an attempt record's header carries, `none` when
/// the record omits it.
fn outcome_of(lines: &[&str]) -> String {
    lines
        .iter()
        .find_map(|line| line.trim().strip_prefix("outcome:"))
        .map_or_else(|| String::from("none"), one_line)
}

/// The first non-blank line after the `marker` line, when the prose has
/// one.
fn line_after<'a>(lines: &[&'a str], marker: &str) -> Option<&'a str> {
    let at = lines.iter().position(|line| line.trim() == marker)?;
    lines[at + 1..]
        .iter()
        .map(|line| line.trim())
        .find(|line| !line.is_empty())
}

/// `text`'s first line, trimmed, `none` when blank.
fn one_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        String::from("none")
    } else {
        line.to_string()
    }
}

/// One body within its share of the budget: whole, cut with a visible
/// count of what was dropped, or omitted by name when almost nothing
/// fits.
fn body_within(body: &str, share: usize) -> String {
    let count = body.chars().count();
    if count <= share {
        return body.to_string();
    }
    if share <= MARK_RESERVE {
        return format!("[body omitted: {count} characters]");
    }
    let kept = share - MARK_RESERVE;
    let head: String = body.chars().take(kept).collect();
    format!("{head}\n[body cut: showing {kept} of {count} characters]")
}

/// One `need_show` answer: the item's identity, kind and status first,
/// then its rendered text within the budget.
fn need_show(answer: &Value, room: usize) -> String {
    let mut out = String::new();
    let item = answer.get("item").unwrap_or(&Value::Null);
    let _ignored = writeln!(out, "handle: {}", field(item, "handle").unwrap_or("none"));
    let _ignored = writeln!(
        out,
        "kind: {}  status: {}",
        field(item, "kind").unwrap_or("none"),
        field(item, "status").unwrap_or("none")
    );
    let _ignored = writeln!(out, "subject: {}", field(item, "subject").unwrap_or(""));
    out.push_str("text:\n");
    let text = answer
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let body = body_within(text, room.saturating_sub(out.chars().count() + 1));
    let _ignored = writeln!(out, "{body}");
    bounded(&out, room)
}

/// A string field of an object, or `None` when absent or not a string.
fn field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// `text` trimmed, whole when within `limit` characters, else cut with a
/// visible `[cut]` marker that fits inside the limit.
fn bounded(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text
        .chars()
        .take(limit.saturating_sub(CUT_MARK.chars().count()))
        .collect();
    format!("{kept}{CUT_MARK}")
}
