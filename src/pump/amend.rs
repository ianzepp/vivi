//! Amendment detection: a planned need whose thread moved after the plan.
//!
//! The board pass compares the newest event timestamp with the need's
//! `thread_seen_at`, hashes the thread only when it moved, and asks the
//! planner for a revision when the hash differs. The plan itself stays
//! immutable on disk; the revision lands in `plans/<need>.r<N>.json`.

use std::collections::BTreeMap;
use std::fs;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::pump::collect::spawn_planner;
use crate::pump::host::Ctx;
use crate::pump::state::State;

/// Detect post-plan thread changes on every planned need.
pub fn detect(ctx: &Ctx, st: &mut State) {
    if st.needs.is_empty() {
        return;
    }
    let events = last_events(ctx);
    expire_revisions(ctx, st);
    if events.is_empty() {
        return;
    }
    let handles: Vec<String> = st.needs.keys().cloned().collect();
    for handle in handles {
        consider(ctx, st, &handle, &events);
    }
}

/// Check one planned need and start a revision when its thread moved.
fn consider(ctx: &Ctx, st: &mut State, handle: &str, events: &BTreeMap<String, u64>) {
    if st.closed_needs.iter().any(|closed| closed == handle)
        || st.board.revision_planning.contains_key(handle)
    {
        return;
    }
    let Some((seen, old_hash)) = st
        .needs
        .get(handle)
        .map(|record| (record.thread_seen_at, record.thread_sha256.clone()))
    else {
        return;
    };
    let Some(record) = st.needs.get(handle) else {
        return;
    };
    if record.amendment_pending || record.verdict != "units" {
        return;
    }
    let Some(last) = events.get(handle).copied() else {
        return;
    };
    if seen.is_some_and(|seen| last <= seen) {
        return;
    }
    let Some(hash) = thread_hash(ctx, handle) else {
        return;
    };
    // A hash from an older scheme (it counted the pump's own replies) is
    // adopted like a first check, never read as an amendment.
    let old_hash = old_hash.filter(|old| old.starts_with(HASH_SCHEME));
    let pending = match old_hash {
        None => {
            // First check after the upgrade: adopt the current thread.
            if let Some(record) = st.needs.get_mut(handle) {
                record.thread_sha256 = Some(hash.clone());
                record.thread_seen_at = Some(last);
            }
            false
        }
        Some(old) if old == hash => {
            if let Some(record) = st.needs.get_mut(handle) {
                record.thread_seen_at = Some(last);
            }
            false
        }
        Some(_) => true,
    };
    if pending {
        if let Some(record) = st.needs.get_mut(handle) {
            record.amendment_pending = true;
            record.thread_sha256 = Some(hash);
            record.thread_seen_at = Some(last);
        }
        launch_revision(ctx, st, handle);
    }
}

/// Epoch seconds of every open need's newest event, from one `board --json`.
fn last_events(ctx: &Ctx) -> BTreeMap<String, u64> {
    let out = ctx.vivi("board --json", 60);
    let Some(start) = out.text.find('{') else {
        return BTreeMap::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&out.text[start..]) else {
        ctx.log("amendment: board json unreadable");
        return BTreeMap::new();
    };
    let mut events = BTreeMap::new();
    for identity in value["identities"].as_array().into_iter().flatten() {
        for need in identity["needs"].as_array().into_iter().flatten() {
            let Some(handle) = need["handle"].as_str() else {
                continue;
            };
            let stamp = need["last_event"]["occurred_at"]
                .as_str()
                .or_else(|| need["date"].as_str());
            if let Some(stamp) = stamp
                && let Some(epoch) = epoch(stamp)
            {
                events.insert(handle.to_owned(), epoch);
            }
        }
    }
    events
}

/// Prefix of the current thread-hash scheme: operator messages only.
const HASH_SCHEME: &str = "v2:";

/// SHA-256 of non-pump messages' ids and bodies, in order, as `v2:<hex>`.
fn thread_hash(ctx: &Ctx, handle: &str) -> Option<String> {
    let text = ctx.vivi(&format!("need show {handle} --json"), 60).text;
    let start = text.find('[')?;
    let messages = match serde_json::from_str::<Vec<Value>>(&text[start..]) {
        Ok(messages) => messages,
        Err(_) => return None,
    };
    let mut hasher = Sha256::new();
    for message in messages {
        let from = message["from"].as_str().unwrap_or("");
        let sender = if ctx.cfg.reply_from.contains('@') {
            from
        } else {
            from.split('@').next().unwrap_or(from)
        };
        if sender.eq_ignore_ascii_case(&ctx.cfg.reply_from) {
            continue;
        }
        hasher.update(message["message_id"].as_str().unwrap_or("").as_bytes());
        hasher.update(b"\n");
        hasher.update(message["body"].as_str().unwrap_or("").as_bytes());
        hasher.update(b"\n");
    }
    Some(format!("{HASH_SCHEME}{}", hex::encode(hasher.finalize())))
}

/// Write the revision prompt and spawn the planner for one need.
fn launch_revision(ctx: &Ctx, st: &mut State, handle: &str) {
    let Some(record) = st.needs.get(handle) else {
        return;
    };
    let revision = record.revision.saturating_add(1);
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
    drop(write_json(
        &needs.join(format!("{handle}.json")),
        &RevisionFile {
            need: handle,
            revision,
            thread: &thread,
            units: &units,
        },
    ));
    let home = ctx.home.to_string_lossy();
    let prompt = format!(
        "Revise one plan, as your system prompt describes. The need thread is in {home}/needs/{handle}.json \
         (keys: need, revision, thread, units). Write {home}/plans/{handle}.r{revision}.json with \
         \"revision\": {revision} and the complete unit set. Keep every existing unit id. Never change a \
         merged, no-defect, inflight or delivered unit; amend only pending units; add new units for the \
         addendum; list withdrawn pending unit ids in \"withdraw\". Your final message is one line: handle, \
         revision, number of units."
    );
    if ctx.cfg.planner.run.is_empty() {
        ctx.log(&format!(
            "amendment: {handle} needs a revision but [pump.planner] run is empty"
        ));
        return;
    }
    spawn_planner(ctx, "board", &format!("{handle}.revision"), &prompt);
    st.board
        .revision_planning
        .insert(handle.to_owned(), ctx.now());
    ctx.log(&format!(
        "amendment: {handle} changed after r{}; revision planner started",
        record.revision
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
                || now.saturating_sub(**at) > timeout
        })
        .map(|(handle, _)| handle.clone())
        .collect();
    for handle in expired {
        let landed = revision_path(ctx, st, &handle).is_some_and(|path| path.exists());
        if !landed {
            ctx.exception(
                "revision-no-answer",
                &handle,
                "the revision planner wrote no plan before the timeout",
            );
        }
        st.board.revision_planning.remove(&handle);
    }
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
