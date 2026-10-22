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
    reask_lost_revisions(ctx, st);
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
        // No trustworthy prior snapshot exists: unset, malformed, or a v3
        // hash that still counted lifecycle event letters as thread changes.
        // Establish today's source hash as the baseline; only later changes
        // can trigger a revision.
        _ => false,
    };
    if pending && withhold_revision(ctx, st, handle) {
        if let Some(record) = st.needs.get_mut(handle) {
            record.thread_sha256 = Some(hash);
            record.thread_seen_at = Some(last);
        }
    } else if pending {
        reopen_closed(ctx, st, handle);
        crate::pump::record::ensure_open(ctx, handle);
        if let Some(record) = st.needs.get_mut(handle) {
            record.amendment_pending = true;
            record.thread_sha256 = Some(hash);
            record.thread_seen_at = Some(last);
        }
        launch_revision(ctx, st, handle, None);
    } else if let Some(record) = st.needs.get_mut(handle) {
        // This also upgrades a matching, proven-v2 snapshot to the current
        // scheme and re-baselines an older v3 snapshot without a revision.
        record.thread_sha256 = Some(hash);
        record.thread_seen_at = Some(last);
    }
}

/// Whether a thread change on `handle` must not reopen or revise the need.
/// The caller re-baselines the hash instead, so the same change is not seen
/// again on the next pass.
///
/// A withdrawn need stays withdrawn. A need closed in the mailspace that the
/// pump has not itself closed was closed by an operator, so a later thread
/// change does not undo that. A need in `closed_needs` keeps the amendment
/// path: the pump closes the mailspace need after landing, and a genuine
/// operator message after that is the feature.
fn withhold_revision(ctx: &Ctx, st: &State, handle: &str) -> bool {
    if st.needs.get(handle).is_some_and(|record| record.withdrawn) {
        ctx.log(&format!(
            "amendment: {handle} thread changed while withdrawn; not reopened"
        ));
        return true;
    }
    if st.closed_needs.iter().any(|closed| closed == handle) {
        return false;
    }
    let closed =
        crate::pump::board::done_need_handles(ctx).is_some_and(|done| done.contains(handle));
    if closed {
        ctx.log(&format!(
            "amendment: {handle} thread changed while closed in the mailspace; not revised"
        ));
    }
    closed
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

/// Prefix for the source-aware thread hash. v4 excludes lifecycle event
/// letters; a stored v3 hash counted them and is re-baselined, not compared.
pub(super) const HASH_SCHEME: &str = "v4:";
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
            .is_some_and(|body| body.starts_with("pump: "))
}

fn legacy_operator_thread_hash(ctx: &Ctx, messages: &[Value]) -> String {
    let mut hasher = Sha256::new();
    for message in messages {
        if is_event_letter(message) || configured_pump_sender(ctx, message) {
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
            && !is_event_letter(message)
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
    let units = revision_units(st, record);
    let Some(prompt) = revision_prompt(ctx, handle, revision, &thread, &units, note) else {
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
    st.board
        .revision_planning
        .insert(handle.to_owned(), queued_at);
    ctx.log(&format!(
        "amendment: {handle} changed after r{}; revision planner started",
        previous_revision
    ));
}

fn revision_units(st: &State, record: &crate::pump::state::NeedRecord) -> Vec<Value> {
    record
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
        .collect()
}

fn revision_prompt(
    ctx: &Ctx,
    handle: &str,
    revision: u64,
    thread: &str,
    units: &[Value],
    note: Option<&str>,
) -> Option<String> {
    let needs = ctx.home.join("needs");
    drop(fs::create_dir_all(&needs));
    let input = RevisionFile {
        need: handle,
        revision,
        thread,
        units,
    };
    let saved = needs.join(format!("{handle}.json"));
    if let Err(error) = write_json(&saved, &input) {
        ctx.exception(
            "revision-input",
            handle,
            &format!("could not save revision input: {error}"),
        );
        return None;
    }
    let input_json = match fitted_input_json(handle, revision, thread, units, &saved) {
        Ok(text) => text,
        Err(error) => {
            ctx.exception(
                "revision-input",
                handle,
                &format!("could not encode revision input: {error}"),
            );
            return None;
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
    Some(match note {
        Some(note) => format!("{prompt} Reason for this revision: {note}."),
        None => prompt,
    })
}

/// The most bytes of revision input JSON that go into the planner prompt. The
/// task letter is capped at 64 KiB, so the prompt carries what fits and the
/// full input stays in `<home>/needs/<handle>.json`.
const REVISION_INPUT_BUDGET: usize = 44_000;

/// The longest unit brief kept when the units alone would crowd the thread out.
const UNIT_BRIEF_CHARS: usize = 600;

/// Encode the revision input, cutting the need thread (and, when the units
/// alone are over half the budget, each unit brief) until it fits
/// [`REVISION_INPUT_BUDGET`]. A cut thread ends with a marker naming where the
/// full text is.
fn fitted_input_json(
    handle: &str,
    revision: u64,
    thread: &str,
    units: &[Value],
    saved: &std::path::Path,
) -> Result<String, serde_json::Error> {
    let encode = |thread: &str, units: &[Value]| {
        serde_json::to_string(&RevisionFile {
            need: handle,
            revision,
            thread,
            units,
        })
    };
    let whole = encode(thread, units)?;
    if whole.len() <= REVISION_INPUT_BUDGET {
        return Ok(whole);
    }
    let mut units = units.to_vec();
    if serde_json::to_string(&units)?.len() > REVISION_INPUT_BUDGET / 2 {
        for unit in &mut units {
            shorten_brief(unit);
        }
    }
    let marker = format!(
        "\n[truncated; full thread via `vivi need show {handle}` or in {}]",
        saved.display()
    );
    let spare = REVISION_INPUT_BUDGET
        .saturating_sub(serde_json::to_string(&units)?.len())
        .max(4_000);
    let mut cut = thread.len().min(spare.saturating_sub(marker.len()));
    loop {
        while !thread.is_char_boundary(cut) {
            cut -= 1;
        }
        let encoded = encode(&format!("{}{marker}", &thread[..cut]), &units)?;
        // JSON escaping makes the encoded size differ from the raw size, so
        // shrink by the overshoot (at least an eighth) until it fits.
        if encoded.len() <= REVISION_INPUT_BUDGET || cut == 0 {
            return Ok(encoded);
        }
        let overshoot = encoded.len() - REVISION_INPUT_BUDGET;
        cut -= overshoot.max(cut / 8).min(cut);
    }
}

fn shorten_brief(unit: &mut Value) {
    let Some(brief) = unit.get("brief").and_then(Value::as_str) else {
        return;
    };
    if brief.chars().count() > UNIT_BRIEF_CHARS {
        let cut: String = brief.chars().take(UNIT_BRIEF_CHARS).collect();
        unit["brief"] = Value::String(format!("{cut} [truncated]"));
    }
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
        if !landed {
            schedule_reask(ctx, st, &handle);
        }
    }
}

/// How many times a revision planner that ended without a usable plan is
/// asked again before the need waits for an operator.
const REVISION_TRIES: u32 = 3;

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

/// Ask again for every revision that was requested and is no longer being
/// answered: the need is `amendment_pending` but no revision planner is in
/// flight, and no answer file is waiting to be ingested. That is the state a
/// failed planner leaves, and the one an older pump left when it lost an
/// answer, so a need can never wait on a revision nobody is producing.
/// Bounded by [`REVISION_TRIES`]; the last failure raises one
/// `revision-failed` exception naming the operator command.
fn reask_lost_revisions(ctx: &Ctx, st: &mut State) {
    let now = ctx.now();
    let lost: Vec<String> = st
        .needs
        .iter()
        .filter(|(handle, record)| {
            record.amendment_pending
                && !record.withdrawn
                && !st.board.revision_planning.contains_key(*handle)
                && !st.board.planning.contains_key(*handle)
                && !answer_waiting(ctx, st, handle)
                && st
                    .board
                    .planner_retry_at
                    .get(&reask_key(handle))
                    .is_none_or(|at| *at <= now)
        })
        .map(|(handle, _)| handle.clone())
        .collect();
    for handle in lost {
        let key = reask_key(&handle);
        let fails = st.board.planner_fails.get(&key).copied().unwrap_or(0);
        if fails > REVISION_TRIES {
            continue;
        }
        if fails == REVISION_TRIES {
            st.board.planner_fails.insert(key.clone(), fails + 1);
            st.board.planner_retry_at.remove(&key);
            ctx.exception("revision-failed", &handle, &given_up_text(st, &handle));
            continue;
        }
        st.board.planner_retry_at.remove(&key);
        // A launch that fails before a planner starts spends a try too, so a
        // broken launch cannot repeat on every pass.
        st.board.planner_fails.insert(key, fails + 1);
        ctx.log(&format!(
            "amendment: {handle} has a requested revision and no planner; asking again"
        ));
        let note = reask_note(st, &handle);
        launch_revision(ctx, st, &handle, Some(&note));
    }
}

/// Why the planner is asked again: the findings of the plan the pump
/// rejected, or that the previous planner gave no usable plan.
fn reask_note(st: &State, handle: &str) -> String {
    st.board.plan_findings.get(handle).map_or_else(
        || String::from("the previous revision planner gave no usable plan"),
        |found| {
            format!(
                "the pump rejected your previous revision; fix every finding and return \
                 the complete plan again. Findings: {found}"
            )
        },
    )
}

/// The `revision-failed` exception text, with the last findings when the
/// planner's answers were rejected.
fn given_up_text(st: &State, handle: &str) -> String {
    let findings = st
        .board
        .plan_findings
        .get(handle)
        .map_or_else(String::new, |found| format!(" Last findings: {found}"));
    format!(
        "{REVISION_TRIES} revision planners gave no usable plan; the need waits \
         for an operator: fix the thread or the plan, then `vivi pump replan {handle}`.{findings}"
    )
}

/// True when the answer file of the next revision sits in `plans/` and the
/// ingest has not consumed it yet.
fn answer_waiting(ctx: &Ctx, st: &State, handle: &str) -> bool {
    let Some(revision) = st.needs.get(handle).map(|r| r.revision.saturating_add(1)) else {
        return false;
    };
    let name = format!("{handle}.r{revision}.json");
    ctx.home.join("plans").join(&name).exists() && !st.plans_seen.iter().any(|seen| seen == &name)
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
