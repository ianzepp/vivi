//! Amendment detection: a planned need whose thread moved after the plan.
//!
//! The board pass hashes each planned need's source thread and asks the
//! planner for a revision when the hash differs. The plan itself stays
//! immutable on disk; the revision lands in `plans/<need>.r<N>.json`.

use std::fs;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::pump::host::{Ctx, shell_quote};
use crate::pump::planner_jobs;
use crate::pump::state::State;

/// Detect post-plan thread changes on every planned need.
pub fn detect(ctx: &Ctx, st: &mut State) {
    if st.needs.is_empty() {
        return;
    }
    expire_revisions(ctx, st);
    let handles: Vec<String> = st.needs.keys().cloned().collect();
    for handle in handles {
        consider(ctx, st, &handle);
    }
}

/// Check one planned need and start a revision when its thread hash moved.
///
/// The thread itself is the source of truth. Board `last_event` is an operator
/// signal, not the trigger: replies used to leave it unchanged, and even when
/// it moves the stamp is often truncated to the same second as the original.
fn consider(ctx: &Ctx, st: &mut State, handle: &str) {
    if st.board.revision_planning.contains_key(handle) {
        return;
    }
    let Some(old_hash) = st
        .needs
        .get(handle)
        .map(|record| record.thread_sha256.clone())
    else {
        return;
    };
    let Some(record) = st.needs.get(handle) else {
        return;
    };
    if record.amendment_pending || record.verdict != "units" {
        return;
    }
    let Some(messages) = thread_messages(ctx, handle) else {
        return;
    };
    let Some(hash) = source_thread_hash(ctx, handle, &messages) else {
        return;
    };
    let last = thread_newest_epoch(&messages).unwrap_or_else(|| ctx.now());
    let pending = match old_hash.as_deref() {
        Some(old) if is_versioned_hash(old, HASH_SCHEME) => old != hash,
        Some(old) if is_versioned_hash(old, LEGACY_HASH_SCHEME) => {
            legacy_operator_thread_hash(ctx, &messages) != old
                || has_untracked_pump_sender_reply(ctx, handle, &messages)
        }
        // No trustworthy prior snapshot exists. Establish today's source
        // hash as the baseline; only later changes can trigger a revision.
        _ => false,
    };
    if pending {
        reopen_closed(ctx, st, handle);
        if let Some(record) = st.needs.get_mut(handle) {
            record.amendment_pending = true;
            record.thread_sha256 = Some(hash);
            record.thread_seen_at = Some(last);
        }
        launch_revision(ctx, st, handle, None);
    } else if let Some(record) = st.needs.get_mut(handle) {
        // This also upgrades a matching, proven-v2 snapshot to v3.
        record.thread_sha256 = Some(hash);
        record.thread_seen_at = Some(last);
    }
}

/// Reopen `handle` when a thread change arrives after the pump closed it.
///
/// The revision's own units re-close the need when they land; until then a
/// closed need that gained thread activity is open work again.
fn reopen_closed(ctx: &Ctx, st: &mut State, handle: &str) {
    if !st.closed_needs.iter().any(|closed| closed == handle) {
        return;
    }
    st.closed_needs.retain(|closed| closed != handle);
    let from = shell_quote(&ctx.cfg.reply_from);
    let out = ctx.vivi(&format!("need reopen {handle} --for {from}"), 60);
    if out.code != 0 {
        ctx.log(&format!(
            "amendment: {handle} mailbox reopen failed: {}",
            out.text.trim()
        ));
    }
    ctx.log(&format!(
        "amendment: {handle} reopened: thread changed after close",
    ));
}

/// Ask for a revision of `handle`'s plan with a reason for the planner, ahead
/// of any thread move.
///
/// `amendment_pending` is set before the planner starts so
/// [`detect`] does not launch a second one; the landing revision clears it.
pub fn request(ctx: &Ctx, st: &mut State, handle: &str, note: &str) {
    let Some(record) = st.needs.get_mut(handle) else {
        return;
    };
    record.amendment_pending = true;
    launch_revision(ctx, st, handle, Some(note));
}

fn thread_newest_epoch(messages: &[Value]) -> Option<u64> {
    messages
        .iter()
        .filter_map(|message| message["date"].as_str().and_then(epoch))
        .max()
}

/// Prefix for the source-aware thread hash.
const HASH_SCHEME: &str = "v3:";
const LEGACY_HASH_SCHEME: &str = "v2:";

fn thread_messages(ctx: &Ctx, handle: &str) -> Option<Vec<Value>> {
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

/// Source snapshot shared by amendment detection, audit jobs and audit
/// freshness checks. Only source messages participate; pump replies require
/// both the configured sender and the durable body marker.
pub(super) fn source_thread_hash(ctx: &Ctx, root: &str, messages: &[Value]) -> Option<String> {
    let mut hasher = Sha256::new();
    for (index, message) in messages.iter().enumerate() {
        if is_generated_pump_reply(ctx, root, index, message) {
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
            .is_some_and(|body| body.starts_with("pump: "))
}

fn legacy_operator_thread_hash(ctx: &Ctx, messages: &[Value]) -> String {
    let mut hasher = Sha256::new();
    for message in messages {
        if configured_pump_sender(ctx, message) {
            continue;
        }
        hasher.update(message["message_id"].as_str().unwrap_or("").as_bytes());
        hasher.update(b"\n");
        hasher.update(message["body"].as_str().unwrap_or("").as_bytes());
        hasher.update(b"\n");
    }
    format!("{LEGACY_HASH_SCHEME}{}", hex::encode(hasher.finalize()))
}

fn has_untracked_pump_sender_reply(ctx: &Ctx, root: &str, messages: &[Value]) -> bool {
    messages.iter().enumerate().any(|(index, message)| {
        !is_root_message(root, index, message)
            && configured_pump_sender(ctx, message)
            && !is_generated_pump_reply(ctx, root, index, message)
    })
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

fn is_root_message(root: &str, index: usize, message: &Value) -> bool {
    let handle = message["handle"].as_str().unwrap_or("");
    handle == root || (handle.is_empty() && index == 0)
}

fn is_versioned_hash(value: &str, scheme: &str) -> bool {
    value.strip_prefix(scheme).is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Write the revision prompt and spawn the planner for one need, naming
/// `note` as the reason when the caller has one.
fn launch_revision(ctx: &Ctx, st: &mut State, handle: &str, note: Option<&str>) {
    let Some(record) = st.needs.get(handle) else {
        return;
    };
    let revision = record.revision.saturating_add(1);
    let previous_revision = record.revision;
    let thread = ctx.vivi(&format!("need show {handle}"), 60).text;
    let thread: String = thread.chars().take(40_000).collect();
    let units: Vec<Value> = record
        .units
        .iter()
        .filter_map(|uid| {
            st.units.get(uid).map(|unit| {
                serde_json::json!({
                    "id": uid,
                    "status": unit.status.as_str(),
                    "target": unit.target_id(),
                    "title": unit.title,
                    "brief": unit.brief,
                    "write_scope": unit.write_scope,
                    "after": unit.after,
                    "proof": unit.proof,
                    "tools": unit.tools,
                })
            })
        })
        .collect();
    let needs = ctx.home.join("needs");
    drop(fs::create_dir_all(&needs));
    let input = RevisionFile {
        need: handle,
        revision,
        thread: &thread,
        units: &units,
    };
    if let Err(error) = write_json(&needs.join(format!("{handle}.json")), &input) {
        ctx.exception(
            "revision-input",
            handle,
            &format!("could not save revision input: {error}"),
        );
        return;
    }
    let input_json = match serde_json::to_string(&input) {
        Ok(text) => text,
        Err(error) => {
            ctx.exception(
                "revision-input",
                handle,
                &format!("could not encode revision input: {error}"),
            );
            return;
        }
    };
    let prompt = format!(
        "Revise one plan, as your system prompt describes. The complete input JSON is between \
         REVISION-INPUT-BEGIN and REVISION-INPUT-END. Return the full plan with revision {revision} \
         and the complete unit set. Keep every existing unit id. Never change a merged, no-defect, \
         inflight or delivered unit; amend only pending units; add new units for the addendum; list \
         withdrawn pending unit ids in \"withdraw\".\n\nREVISION-INPUT-BEGIN\n{input_json}\n\
         REVISION-INPUT-END"
    );
    let prompt = match note {
        Some(note) => format!("{prompt} Reason for this revision: {note}."),
        None => prompt,
    };
    let owner = format!("{handle}.revision");
    let queued_at = ctx.now();
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
    st.board
        .revision_planning
        .insert(handle.to_owned(), queued_at);
    ctx.log(&format!(
        "amendment: {handle} changed after r{}; revision planner started",
        previous_revision
    ));
}

/// Drop revision planners whose plan file landed, or whose timeout expired.
fn expire_revisions(ctx: &Ctx, st: &mut State) {
    let timeout = ctx.cfg.board.plan_timeout;
    let now = ctx.now();
    let expired: Vec<String> = st
        .board
        .revision_planning
        .iter()
        .filter(|(handle, at)| {
            revision_path(ctx, st, handle).is_some_and(|path| path.exists())
                || crate::pump::host::exit_receipt_path(&revision_out_path(ctx, handle)).exists()
                || revision_timed_out(st, handle, **at, now, timeout)
        })
        .map(|(handle, _)| handle.clone())
        .collect();
    for handle in expired {
        let landed = revision_path(ctx, st, &handle).is_some_and(|path| path.exists());
        if !landed {
            let out = revision_out_path(ctx, &handle);
            let receipt = crate::pump::host::exit_receipt_path(&out);
            if receipt.exists() {
                let code = fs::read_to_string(&receipt).unwrap_or_default();
                let output = fs::read_to_string(&out).unwrap_or_default();
                ctx.exception(
                    "revision-no-answer",
                    &handle,
                    &format!(
                        "revision planner exited rc={} without a plan: {}",
                        code.trim(),
                        crate::pump::collect::tail(&output, 200)
                    ),
                );
                drop(fs::remove_file(receipt));
            } else {
                ctx.exception(
                    "revision-no-answer",
                    &handle,
                    "the revision planner wrote no plan before the timeout",
                );
            }
        }
        st.board.revision_planning.remove(&handle);
    }
}

fn revision_timed_out(st: &State, handle: &str, queued_at: u64, now: u64, timeout: u64) -> bool {
    match planner_jobs::lifecycle(st, "revision", &format!("{handle}.revision"), queued_at) {
        Some((_status, Some(started))) => now.saturating_sub(started) > timeout,
        Some((status, None)) => status.is_terminal(),
        None => now.saturating_sub(queued_at) > timeout,
    }
}

fn revision_out_path(ctx: &Ctx, handle: &str) -> std::path::PathBuf {
    ctx.home
        .join("board")
        .join(format!("{handle}.revision.out"))
}

/// The expected revision file for a need, when it has a record.
fn revision_path(ctx: &Ctx, st: &State, handle: &str) -> Option<std::path::PathBuf> {
    let revision = st.needs.get(handle)?.revision.saturating_add(1);
    Some(
        ctx.home
            .join("plans")
            .join(format!("{handle}.r{revision}.json")),
    )
}

/// Parse an RFC 3339 stamp to epoch seconds.
fn epoch(stamp: &str) -> Option<u64> {
    match chrono::DateTime::parse_from_rfc3339(stamp) {
        Ok(time) => Some(u64::try_from(time.timestamp()).unwrap_or(0)),
        Err(_) => None,
    }
}

#[derive(Serialize)]
struct RevisionFile<'a> {
    need: &'a str,
    revision: u64,
    thread: &'a str,
    units: &'a [Value],
}

fn write_json(path: &std::path::Path, value: &impl Serialize) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    fs::write(path, text)
}
