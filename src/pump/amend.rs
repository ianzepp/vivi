//! Plan revisions on request, and the source-thread hash the audit uses.
//!
//! Thread-change detection is the flow's job now; what stays here is the
//! explicit revision request and the thread helpers. The plan itself stays
//! immutable on disk; the revision lands in `plans/<need>.r<N>.json`.

use std::fs;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::pump::host::Ctx;
use crate::pump::planner_jobs;
use crate::pump::revision_input::{self, Inputs, Thread};
use crate::pump::state::State;

/// Ask for a revision of `handle`'s plan with a reason for the planner, ahead
/// of any thread move.
///
/// `amendment_pending` is set before the planner starts; the landing
/// revision clears it.
pub fn request(ctx: &Ctx, st: &mut State, handle: &str, note: &str) {
    let Some(record) = st.needs.get_mut(handle) else {
        return;
    };
    record.amendment_pending = true;
    launch_revision(ctx, st, handle, Some(note));
}

/// Prefix for the source-aware thread hash. v4 excludes lifecycle event
/// letters; a stored v3 hash counted them and is re-baselined, not compared.
pub(super) const HASH_SCHEME: &str = "v4:";
pub(super) fn thread_messages(ctx: &Ctx, handle: &str) -> Option<Vec<Value>> {
    let text = ctx.vivi(&format!("need show {handle} --json"), 60).text;
    let start = text.find('[')?;
    match serde_json::from_str::<Vec<Value>>(&text[start..]) {
        Ok(messages) => Some(messages),
        Err(error) => {
            ctx.log(&format!(
                "amendment: need {handle} thread JSON unreadable: {error}"
            ));
            None
        }
    }
}

/// Source snapshot shared by audit jobs and audit
/// freshness checks. Only source messages participate; pump replies require
/// both the configured sender and the durable body marker, and lifecycle
/// event letters (done, reopened, marked, ...) record state, not content.
pub(super) fn source_thread_hash(ctx: &Ctx, root: &str, messages: &[Value]) -> Option<String> {
    let mut hasher = Sha256::new();
    for (index, message) in messages.iter().enumerate() {
        if is_event_letter(message) || is_generated_pump_reply(ctx, root, index, message) {
            continue;
        }
        let message_id = message["message_id"].as_str()?;
        if message_id.is_empty() {
            return None;
        }
        for field in [
            message["handle"].as_str().unwrap_or(""),
            message_id,
            message["from"].as_str().unwrap_or(""),
            message["subject"].as_str().unwrap_or(""),
            message["body"].as_str().unwrap_or(""),
        ] {
            hasher.update((field.len() as u64).to_be_bytes());
            hasher.update(field.as_bytes());
        }
    }
    Some(format!("{HASH_SCHEME}{}", hex::encode(hasher.finalize())))
}

/// Whether the thread JSON marks `message` as a lifecycle event letter, one
/// stamped with `X-Vivi-Event`.
pub(super) fn is_event_letter(message: &Value) -> bool {
    message["event"]
        .as_str()
        .is_some_and(|event| !event.is_empty())
}

/// A reply is pump-generated only when the configured pump identity authored
/// the message and its body carries the durable pump marker.
pub(super) fn is_generated_pump_reply(
    ctx: &Ctx,
    root: &str,
    index: usize,
    message: &Value,
) -> bool {
    if is_root_message(root, index, message) {
        return false;
    }
    configured_pump_sender(ctx, message)
        && message["body"]
            .as_str()
            .is_some_and(|body| crate::letter::prose_of(body).starts_with("pump: "))
}

fn configured_pump_sender(ctx: &Ctx, message: &Value) -> bool {
    let from = message["from"].as_str().unwrap_or("");
    let sender = if ctx.cfg.reply_from.contains('@') {
        from
    } else {
        from.split('@').next().unwrap_or(from)
    };
    sender.eq_ignore_ascii_case(&ctx.cfg.reply_from)
}

pub(super) fn is_root_message(root: &str, index: usize, message: &Value) -> bool {
    let handle = message["handle"].as_str().unwrap_or("");
    handle == root || (handle.is_empty() && index == 0)
}

/// Write the revision prompt and spawn the planner for one need, naming
/// `note` as the reason when the caller has one.
fn launch_revision(ctx: &Ctx, st: &mut State, handle: &str, note: Option<&str>) {
    let Some(record) = st.needs.get(handle) else {
        return;
    };
    let revision = record.revision.saturating_add(1);
    let previous_revision = record.revision;
    let thread = Thread::fetch(ctx, handle);
    let seen = revision_input::load_seen(ctx, handle, previous_revision);
    let Some(prompt) = revision_prompt(
        ctx,
        st,
        &Inputs {
            need: handle,
            revision,
            thread: &thread,
            seen: seen.as_ref(),
        },
        note,
    ) else {
        return;
    };
    let owner = format!("{handle}.revision");
    let queued_at = ctx.now();
    discard_stale_answer(ctx, st, handle, revision);
    if let Err(error) = planner_jobs::enqueue(
        ctx,
        st,
        "revision",
        &owner,
        queued_at,
        &prompt,
        ctx.cfg.board.plan_timeout,
        &format!("board/{owner}.out"),
        &format!("plans/{handle}.r{revision}.json"),
        None,
    ) {
        ctx.exception("planner-dispatch-failed", handle, &error.to_string());
        return;
    }
    revision_input::save_seen(ctx, handle, revision, &thread);
    st.board
        .revision_planning
        .insert(handle.to_owned(), queued_at);
    ctx.log(&format!(
        "amendment: {handle} changed after r{}; revision planner started",
        previous_revision
    ));
}

/// The planner prompt for one revision. The full input is saved on the pump
/// host; the prompt carries the whole prior plan and as much of the thread as
/// the task letter holds, and says what it left out. `None` when the input
/// cannot be saved or the prior plan alone does not fit the letter.
fn revision_prompt(
    ctx: &Ctx,
    st: &State,
    inputs: &Inputs<'_>,
    note: Option<&str>,
) -> Option<String> {
    let (handle, revision) = (inputs.need, inputs.revision);
    let needs = ctx.home.join("needs");
    drop(fs::create_dir_all(&needs));
    let saved = needs.join(format!("{handle}.json"));
    let full = match revision_input::build(ctx, st, inputs, None) {
        Ok(value) => value,
        Err(error) => {
            ctx.exception("revision-input", handle, &error);
            return None;
        }
    };
    if let Err(error) = write_json(&saved, &full) {
        ctx.exception(
            "revision-input",
            handle,
            &format!("could not save revision input: {error}"),
        );
        return None;
    }
    let input_json = match revision_input::build(ctx, st, inputs, Some(revision_input::BUDGET)) {
        Ok(value) => value.to_string(),
        Err(error) => {
            ctx.exception("revision-input", handle, &error);
            return None;
        }
    };
    let prompt = format!(
        "Revise one plan, as your system prompt describes. The complete input JSON is between \
         REVISION-INPUT-BEGIN and REVISION-INPUT-END. `units` is the whole prior plan: every unit \
         with its full brief, write scope and proof, and a `state` that says what you may do with \
         it. Return the full plan with revision {revision} and the complete unit set. Keep every \
         existing unit id. Copy every unit whose `amendable` is false exactly as given (merged, \
         no-defect, in-flight, delivered and the like); never change a withdrawn or replaced unit \
         or reuse its id; amend only units whose state is `pending`; add new units for the \
         addendum; list withdrawn pending unit ids in \"withdraw\". `thread` holds the need \
         thread; read `thread_note` for which messages are new since the last plan and \
         `thread_elided` for anything left out.\n\nREVISION-INPUT-BEGIN\n{input_json}\n\
         REVISION-INPUT-END"
    );
    Some(match note {
        Some(note) => format!("{prompt} Reason for this revision: {note}."),
        None => prompt,
    })
}

/// Seconds before the first re-ask; it doubles per failure.
const REVISION_BACKOFF: u64 = 60;

/// The `planner_fails` / `planner_retry_at` key of a need's revision planner.
fn reask_key(handle: &str) -> String {
    format!("{handle}.revision")
}

/// A revision planner ended without landing a plan: count it and set the
/// earliest time [`reask_lost_revisions`] may ask again. The need stays
/// `amendment_pending`, so nothing dispatches against a plan that is known to
/// be stale until a revision lands or an operator steps in.
pub(super) fn schedule_reask(ctx: &Ctx, st: &mut State, handle: &str) {
    if !st
        .needs
        .get(handle)
        .is_some_and(|record| record.amendment_pending)
    {
        return;
    }
    let key = reask_key(handle);
    let fails = st.board.planner_fails.entry(key.clone()).or_insert(0);
    *fails += 1;
    let fails = *fails;
    let delay = REVISION_BACKOFF.saturating_mul(1u64 << fails.saturating_sub(1).min(16));
    st.board
        .planner_retry_at
        .insert(key, ctx.now().saturating_add(delay));
}

/// Move aside an answer file of `revision` that was consumed without taking
/// effect, and forget it, so the new planner's file is read as a new plan.
fn discard_stale_answer(ctx: &Ctx, st: &mut State, handle: &str, revision: u64) {
    let name = format!("{handle}.r{revision}.json");
    st.plans_seen.retain(|seen| seen != &name);
    let path = ctx.home.join("plans").join(&name);
    if !path.exists() {
        return;
    }
    let dir = ctx.home.join("plans/superseded");
    drop(fs::create_dir_all(&dir));
    if fs::rename(&path, dir.join(format!("{name}.{}", ctx.now()))).is_err() {
        drop(fs::remove_file(&path));
    }
}

fn write_json(path: &std::path::Path, value: &impl Serialize) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    fs::write(path, text)
}
