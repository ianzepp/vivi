//! The input a revision planner is given.
//!
//! A revision must keep every unit that already landed or is in flight exactly
//! as it was, so the planner is shown the whole prior plan: every unit of the
//! need with its full brief and a `state` label that says whether it may be
//! amended. The need thread comes as structured messages, each marked when it
//! is new since the last plan.
//!
//! The task letter is capped at 64 KiB, so the prompt copy has a byte budget.
//! Only the thread history gives way to it: oldest messages are left out first
//! and a very long message is cut, and `thread_elided` says what went. A unit
//! brief is never cut. Briefs of closed (withdrawn or replaced) units are the
//! one thing dropped when the prior plan alone would not fit, and a plan that
//! still does not fit is refused loudly instead of being sent short.
//!
//! The full, uncapped input is saved to `<home>/needs/<need>.json`.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::pump::amend::{is_event_letter, is_generated_pump_reply, is_root_message};
use crate::pump::host::Ctx;
use crate::pump::state::{State, Status, Unit};

/// The most bytes of revision input JSON that go into the planner prompt. The
/// task letter is capped at 64 KiB; the rest is the prompt's own text and the
/// pool result contract.
pub(super) const BUDGET: usize = 50_000;

/// Bytes always kept for the thread, so a large prior plan cannot crowd out
/// the need and what changed.
const THREAD_FLOOR: usize = 4_000;

/// Bytes the envelope (need, revision, notes, elision report) takes.
const ENVELOPE: usize = 1_500;

/// The least body length worth keeping when a message must be cut.
const MIN_BODY: usize = 800;

/// The head of a raw-text thread that is always kept.
const TEXT_HEAD: usize = 2_000;

/// The need thread as the pump read it.
pub(super) enum Thread {
    /// Parsed `vivi need show --json` messages.
    Messages(Vec<Value>),
    /// The `vivi need show` text, when the JSON could not be read.
    Text(String),
}

impl Thread {
    /// Read the thread of `handle`, structured when possible.
    pub(super) fn fetch(ctx: &Ctx, handle: &str) -> Thread {
        match crate::pump::amend::thread_messages(ctx, handle) {
            Some(messages) => Thread::Messages(messages),
            None => Thread::Text(ctx.vivi(&format!("need show {handle}"), 60).text),
        }
    }
}

/// What one revision input is built from.
pub(super) struct Inputs<'a> {
    pub need: &'a str,
    pub revision: u64,
    pub thread: &'a Thread,
    /// Message ids the previous revision's planner was shown, when known.
    pub seen: Option<&'a BTreeSet<String>>,
}

/// Build the revision input: the whole prior plan and the thread. `budget`
/// bounds the encoded size for a prompt; `None` builds the full copy.
///
/// # Errors
/// Returns the reason when the prior plan alone does not fit the budget.
pub(super) fn build(
    ctx: &Ctx,
    st: &State,
    inputs: &Inputs<'_>,
    budget: Option<usize>,
) -> Result<Value, String> {
    let mut units = prior_units(st, inputs.need);
    let mut thread_room = None;
    if let Some(budget) = budget {
        let room = budget.saturating_sub(ENVELOPE);
        fit_units(&mut units, room.saturating_sub(THREAD_FLOOR), inputs.need)?;
        thread_room = Some(room.saturating_sub(encoded_len(&units)));
    }
    let (thread, note, elided) = match inputs.thread {
        Thread::Messages(messages) => thread_entries(ctx, inputs, messages, thread_room),
        Thread::Text(text) => thread_text(inputs.need, text, thread_room),
    };
    let mut out = json!({
        "need": inputs.need,
        "revision": inputs.revision,
        "units": units,
        "thread": thread,
        "thread_note": note,
    });
    if let Some(elided) = elided {
        out["thread_elided"] = elided;
    }
    Ok(out)
}

fn encoded_len(value: &impl serde::Serialize) -> usize {
    serde_json::to_string(value).map_or(0, |text| text.len())
}

// -- the prior plan ---------------------------------------------------------

/// Every unit of the need: the live record's units in order, then units of
/// the need the record no longer lists (withdrawn or replaced by a revision).
fn prior_units(st: &State, need: &str) -> Vec<Value> {
    let listed: &[String] = st.needs.get(need).map_or(&[], |record| &record.units);
    let others = st
        .units
        .iter()
        .filter(|(uid, unit)| unit.need == need && !listed.contains(uid))
        .map(|(uid, _)| uid);
    listed
        .iter()
        .chain(others)
        .filter_map(|uid| st.units.get(uid).map(|unit| unit_json(uid, unit)))
        .collect()
}

fn unit_json(uid: &str, unit: &Unit) -> Value {
    let (state, amendable) = unit_state(unit.status);
    json!({
        "id": uid,
        "state": state,
        "amendable": amendable,
        "status": unit.status.as_str(),
        "revision": unit.revision,
        "target": unit.target_id(),
        "title": unit.title,
        "brief": unit.brief,
        "write_scope": unit.write_scope,
        "after": unit.after,
        "proof": unit.proof,
        "tools": unit.tools,
    })
}

/// The state label the planner reads, and whether a revision may change the
/// unit. Mirrors what [`crate::pump::plan`] applies: only a unit that has not
/// started may be amended; a landed or running unit must come back unchanged;
/// a withdrawn or replaced unit is closed.
fn unit_state(status: Status) -> (&'static str, bool) {
    match status {
        Status::Pending | Status::Held | Status::NeedsReplan => ("pending", true),
        Status::Merged | Status::NoDefect => ("merged", false),
        Status::Withdrawn => ("withdrawn", false),
        Status::Replaced => ("replaced", false),
        _ => ("in-flight", false),
    }
}

/// Make the prior plan fit `room`: closed units give up their briefs (they
/// are no longer work), and a plan that still does not fit is refused.
fn fit_units(units: &mut [Value], room: usize, need: &str) -> Result<(), String> {
    if encoded_len(&units) <= room {
        return Ok(());
    }
    for unit in units.iter_mut() {
        let closed = matches!(unit["state"].as_str(), Some("withdrawn" | "replaced"));
        if closed {
            unit["brief"] = Value::String(String::from(
                "[brief left out: this unit is closed and its text is not needed to revise]",
            ));
            unit["brief_elided"] = Value::Bool(true);
        }
    }
    let size = encoded_len(&units);
    if size <= room {
        return Ok(());
    }
    Err(format!(
        "the prior plan of {need} is {size} bytes with every live brief in full, over the \
         {room} bytes a planner letter leaves for it; split the need or withdraw units, then \
         `vivi pump replan {need}`"
    ))
}

// -- the thread -------------------------------------------------------------

/// One thread message ready to send: its place in the thread, its encoding,
/// whether it is new since the last plan, and how it ranks for the budget.
struct Entry {
    index: usize,
    value: Value,
    rank: u8,
}

#[derive(Default)]
struct LeftOut {
    event_letters: usize,
    pump_replies: usize,
}

/// Which messages count as new since the last plan, and how that was decided.
enum Basis<'a> {
    /// The ids the previous planner was shown.
    Seen(&'a BTreeSet<String>),
    /// Everything after this message index, the pump's last planned/revised
    /// reply.
    Marker(usize),
    None,
}

fn thread_entries(
    ctx: &Ctx,
    inputs: &Inputs<'_>,
    messages: &[Value],
    room: Option<usize>,
) -> (Value, String, Option<Value>) {
    let need = inputs.need;
    let basis = inputs.seen.map_or_else(
        || last_plan_marker(ctx, need, messages).map_or(Basis::None, Basis::Marker),
        Basis::Seen,
    );
    let mut left = LeftOut::default();
    let mut entries = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if is_event_letter(message) {
            left.event_letters += 1;
        } else if is_generated_pump_reply(ctx, need, index, message) {
            left.pump_replies += 1;
        } else {
            entries.push(entry(need, index, message, &basis));
        }
    }
    let (chosen, omitted, truncated) = select(entries, room);
    let elided = elision(need, &left, omitted, truncated);
    (Value::Array(chosen), basis_note(&basis), elided)
}

/// The index of the pump's newest `planned` or `revised` reply, if any.
fn last_plan_marker(ctx: &Ctx, need: &str, messages: &[Value]) -> Option<usize> {
    messages
        .iter()
        .enumerate()
        .filter(|(index, message)| {
            is_generated_pump_reply(ctx, need, *index, message)
                && message["body"].as_str().is_some_and(|body| {
                    let prose = crate::letter::prose_of(body);
                    prose.starts_with("pump: planned") || prose.starts_with("pump: revised")
                })
        })
        .map(|(index, _)| index)
        .max()
}

fn entry(need: &str, index: usize, message: &Value, basis: &Basis<'_>) -> Entry {
    let root = is_root_message(need, index, message);
    let new = match basis {
        Basis::Seen(seen) => Some(
            !message["message_id"]
                .as_str()
                .is_some_and(|id| seen.contains(id)),
        ),
        Basis::Marker(marker) => Some(index > *marker),
        Basis::None => None,
    };
    let mut value = json!({
        "handle": message["handle"].as_str().unwrap_or(""),
        "date": message["date"].as_str().unwrap_or(""),
        "from": message["from"].as_str().unwrap_or(""),
        "kind": if root { "need" } else { "reply" },
        "body": message["body"].as_str().unwrap_or(""),
    });
    if root {
        value["subject"] = json!(message["subject"].as_str().unwrap_or(""));
    }
    if let Some(new) = new {
        value["since_last_plan"] = Value::Bool(new);
    }
    let rank = if root {
        0
    } else if new == Some(true) {
        1
    } else {
        2
    };
    Entry { index, value, rank }
}

/// The messages that go in, in thread order, with the count left out and the
/// count cut short. The need itself ranks first, then new messages, newest
/// first, then older ones, newest first.
fn select(mut entries: Vec<Entry>, room: Option<usize>) -> (Vec<Value>, usize, usize) {
    let Some(room) = room else {
        return (entries.into_iter().map(|e| e.value).collect(), 0, 0);
    };
    entries.sort_by_key(|e| (e.rank, std::cmp::Reverse(e.index)));
    let mut remaining = room.saturating_sub(2);
    let (mut chosen, mut omitted, mut truncated) = (Vec::new(), 0, 0);
    for entry in entries {
        let len = encoded_len(&entry.value) + 1;
        if len <= remaining {
            remaining -= len;
            chosen.push((entry.index, entry.value));
        } else if entry.rank <= 1
            && let Some(cut) = cut_body(&entry.value, remaining)
        {
            remaining = remaining.saturating_sub(encoded_len(&cut) + 1);
            truncated += 1;
            chosen.push((entry.index, cut));
        } else {
            omitted += 1;
        }
    }
    chosen.sort_by_key(|(index, _)| *index);
    (
        chosen.into_iter().map(|(_, value)| value).collect(),
        omitted,
        truncated,
    )
}

/// `value` with its body cut so the entry encodes within `room`, or `None`
/// when not even a short piece of the body fits.
fn cut_body(value: &Value, room: usize) -> Option<Value> {
    let body = value["body"].as_str()?;
    let total = body.chars().count();
    let mut keep = total.min(room);
    while keep >= MIN_BODY.min(total) && keep > 0 {
        let head: String = body.chars().take(keep).collect();
        let mut cut = value.clone();
        cut["body"] = Value::String(format!(
            "{head}\n[body cut: kept {keep} of {total} characters]"
        ));
        if encoded_len(&cut) < room {
            return Some(cut);
        }
        keep -= (keep / 8).max(1);
    }
    None
}

fn basis_note(basis: &Basis<'_>) -> String {
    match basis {
        Basis::Seen(_) => String::from(
            "`since_last_plan` is true for a message the planner of the last plan was not shown.",
        ),
        Basis::Marker(_) => String::from(
            "`since_last_plan` is true for a message after the pump's last planned/revised reply; \
             a message posted while that reply was pending can be marked old, so check the \
             thread against the prior plan.",
        ),
        Basis::None => String::from(
            "No marker of the last plan was found, so no message is flagged; compare the thread \
             with the prior plan.",
        ),
    }
}

fn elision(need: &str, left: &LeftOut, omitted: usize, truncated: usize) -> Option<Value> {
    if left.event_letters + left.pump_replies + omitted + truncated == 0 {
        return None;
    }
    Some(json!({
        "messages_left_out": omitted,
        "bodies_cut": truncated,
        "lifecycle_event_letters_left_out": left.event_letters,
        "pump_replies_left_out": left.pump_replies,
        "full_thread": format!("vivi need show {need}"),
    }))
}

/// A raw-text thread: the head (the need itself) and the tail (the newest
/// activity), with the middle named as left out when it must go.
fn thread_text(need: &str, text: &str, room: Option<usize>) -> (Value, String, Option<Value>) {
    let note = String::from(
        "The structured thread was unreadable; `thread` is the raw text, oldest first.",
    );
    let Some(room) = room.filter(|room| text.len() > *room) else {
        return (Value::String(text.to_owned()), note, None);
    };
    let marker =
        format!("\n[middle of the thread left out; full thread via `vivi need show {need}`]\n");
    let spare = room.saturating_sub(marker.len() + 200);
    let head = text_prefix(text, TEXT_HEAD.min(spare / 2));
    let tail = text_suffix(text, spare.saturating_sub(head.len()));
    let kept = Value::String(format!("{head}{marker}{tail}"));
    let elided = json!({
        "text_bytes_left_out": text.len().saturating_sub(head.len() + tail.len()),
        "full_thread": format!("vivi need show {need}"),
    });
    (kept, note, Some(elided))
}

fn text_prefix(text: &str, len: usize) -> &str {
    let mut end = len.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn text_suffix(text: &str, len: usize) -> &str {
    let mut start = text.len().saturating_sub(len);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

// -- what the last planner was shown ------------------------------------------

fn seen_path(ctx: &Ctx, need: &str, revision: u64) -> PathBuf {
    ctx.home
        .join("needs")
        .join(format!("{need}.r{revision}.seen.json"))
}

/// The message ids the planner of `revision` was shown, when this pump saved
/// them (first plans have none).
pub(super) fn load_seen(ctx: &Ctx, need: &str, revision: u64) -> Option<BTreeSet<String>> {
    let path = seen_path(ctx, need, revision);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            ctx.log(&format!(
                "revision seen set {} unreadable: {error}",
                path.display()
            ));
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(seen) => Some(seen),
        Err(error) => {
            ctx.log(&format!(
                "revision seen set {} is invalid: {error}",
                path.display()
            ));
            None
        }
    }
}

/// Remember the message ids the planner of `revision` is shown, so the next
/// revision can mark what arrived after. A text thread has no ids to keep.
pub(super) fn save_seen(ctx: &Ctx, need: &str, revision: u64, thread: &Thread) {
    let Thread::Messages(messages) = thread else {
        return;
    };
    let ids: BTreeSet<&str> = messages
        .iter()
        .filter_map(|message| message["message_id"].as_str())
        .collect();
    let path = seen_path(ctx, need, revision);
    if let Some(parent) = path.parent() {
        drop(fs::create_dir_all(parent));
    }
    match serde_json::to_string(&ids) {
        Ok(text) => {
            if let Err(error) = fs::write(&path, text) {
                ctx.log(&format!(
                    "revision input: could not save {}: {error}",
                    path.display()
                ));
            }
        }
        Err(error) => ctx.log(&format!(
            "revision input: could not encode seen ids: {error}"
        )),
    }
}

#[cfg(test)]
#[path = "revision_input_test.rs"]
mod tests;
