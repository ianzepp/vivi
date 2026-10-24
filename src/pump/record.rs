//! The pump's side of pump records.
//!
//! One dispatch of a need is one pump record (see
//! [`crate::mailspace::PumpRecord`]). The pump plans exactly the needs that
//! have an open record, posts its progress as replies on that record's
//! thread, and closes the record with an outcome word when the dispatch ends
//! (`landed` when the need closed and every unit of it was satisfied).
//!
//! Reads (which records are open) go through the `vivi` command seam like
//! every other board read. Closing and opening call the mailspace library
//! directly. Nothing here fails the tick: a lookup or a close that cannot run
//! is logged and the next pass tries again.

use std::collections::BTreeSet;
use std::fs;

use serde_json::Value;

use crate::mailspace::{Mailspace, PumpState};
use crate::pump::host::{Ctx, shell_quote};

/// The dispatch ended with the need closed and all its work merged.
pub const LANDED: &str = "landed";

/// The open pump records as `vivi pump list --status open --json` prints
/// them, or `None` when the list cannot be read.
fn open_records(ctx: &Ctx, need: Option<&str>) -> Option<Vec<Value>> {
    let filter = need.map_or_else(String::new, |need| format!(" --need {}", shell_quote(need)));
    let out = ctx.vivi(&format!("pump list --status open --json{filter}"), 60);
    let json = out.text.find('[').map(|start| &out.text[start..]);
    if let (0, Some(json)) = (out.code, json)
        && let Ok(records) = serde_json::from_str::<Vec<Value>>(json)
    {
        return Some(records);
    }
    let tail: String = out.text.trim().chars().take(150).collect();
    ctx.log(&format!("pump record list unreadable: {tail}"));
    None
}

/// Handles of the needs that have an open pump record, or `None` when the
/// list is unreadable.
#[must_use]
pub fn open_need_handles(ctx: &Ctx) -> Option<BTreeSet<String>> {
    Some(
        open_records(ctx, None)?
            .iter()
            .filter_map(|record| record.get("need").and_then(Value::as_str))
            .map(String::from)
            .collect(),
    )
}

/// The handle of `need`'s open pump record, if it has one.
#[must_use]
pub fn open_handle(ctx: &Ctx, need: &str) -> Option<String> {
    open_records(ctx, Some(need))?
        .iter()
        .find_map(|record| record.get("handle").and_then(Value::as_str))
        .map(String::from)
}

/// Close `need`'s open pump record with `outcome` as its note. A non-empty
/// `detail` is posted first as a reply on the record, so the reason is on its
/// thread. Returns the closed handle; `None` when the need has no open
/// record or the mailspace cannot be reached (logged).
pub fn finish(ctx: &Ctx, need: &str, outcome: &str, detail: &str) -> Option<String> {
    let mailspace = open_mailspace(ctx)?;
    let open = match mailspace.pump_list(Some(need), Some(PumpState::Open)) {
        Ok(open) => open,
        Err(error) => {
            ctx.log(&format!("pump record lookup for {need} failed: {error}"));
            return None;
        }
    };
    let record = open.into_iter().next()?;
    close_record(ctx, &mailspace, &record.handle, need, outcome, detail)
}

/// Close one record, replying `detail` first when there is some.
pub(crate) fn close_record(
    ctx: &Ctx,
    mailspace: &Mailspace,
    handle: &str,
    need: &str,
    outcome: &str,
    detail: &str,
) -> Option<String> {
    if !detail.trim().is_empty() {
        reply_on(ctx, handle, need, outcome, detail);
    }
    match mailspace.pump_done(handle, Some(outcome)) {
        Ok(record) => {
            ctx.log(&format!(
                "pump record {} closed for need {need}: {outcome}",
                record.handle
            ));
            Some(record.handle)
        }
        Err(error) => {
            ctx.log(&format!(
                "pump record {handle} for need {need} not closed ({outcome}): {error}"
            ));
            None
        }
    }
}

pub(crate) fn open_mailspace(ctx: &Ctx) -> Option<Mailspace> {
    match Mailspace::discover(Some(&ctx.project)) {
        Ok(mailspace) => Some(mailspace),
        Err(error) => {
            ctx.log(&format!("pump records unavailable: {error}"));
            None
        }
    }
}

/// Post a pump progress reply on `need`'s open pump record. A need with no
/// open record gets nothing: the reply is logged and dropped (false).
pub fn send_reply(ctx: &Ctx, need: &str, stage: &str, body: &str) -> bool {
    let Some(record) = crate::pump::record::open_handle(ctx, need) else {
        ctx.log(&format!(
            "board reply {need} {stage} dropped: no open pump record"
        ));
        return false;
    };
    reply_on(ctx, &record, need, stage, body)
}

/// Post a reply on the need's own thread: only for a message the operator
/// must act on (a plan verdict that waits for a ruling).
pub fn send_need_reply(ctx: &Ctx, need: &str, stage: &str, body: &str) -> bool {
    reply_on(ctx, need, need, stage, body)
}

/// Write the body to `<home>/board/reply-<need>-<stage>.txt` and run `mail
/// reply <target>` from the reply identity; true when the reply succeeded.
/// `target` is a pump record or a need handle. The recipient is
/// `board.reply_to` when set; see [`reply_on_task`] for a pump-to-pump task.
pub fn reply_on(ctx: &Ctx, target: &str, need: &str, stage: &str, body: &str) -> bool {
    let reply_to = ctx.cfg.reply_to();
    reply_with(ctx, target, need, stage, body, &reply_to)
}

/// Post a reply on a task the pump sent to itself (a flow step, a slice, the
/// main-rung task). The only other identity on that thread is the pump, so the
/// reply always goes `--to <reply_from>`: a bare `mail reply` has no other
/// recipient to resolve, and `board.reply_to` would send it to the operator.
pub fn reply_on_task(ctx: &Ctx, task: &str, need: &str, stage: &str, body: &str) -> bool {
    reply_with(ctx, task, need, stage, body, &ctx.cfg.reply_from)
}

/// The body-file and log code shared by [`reply_on`] and [`reply_on_task`]:
/// `reply_to` is the `--to` value, left off when empty.
fn reply_with(
    ctx: &Ctx,
    target: &str,
    need: &str,
    stage: &str,
    body: &str,
    reply_to: &str,
) -> bool {
    let dir = ctx.home.join("board");
    drop(fs::create_dir_all(&dir));
    let path = dir.join(format!("reply-{need}-{stage}.txt"));
    if fs::write(&path, format!("{body}\n")).is_err() {
        ctx.log(&format!(
            "board reply {need} {stage} rc=1 body file unwritable"
        ));
        return false;
    }
    let mut args = format!(
        "mail reply {} --from {}",
        shell_quote(target),
        shell_quote(&ctx.cfg.reply_from)
    );
    if !reply_to.is_empty() {
        args.push_str(&format!(" --to {}", shell_quote(reply_to)));
    }
    args.push_str(&format!(
        " --body-file {}",
        shell_quote(&path.to_string_lossy())
    ));
    let out = ctx.vivi(&args, 60);
    ctx.log(&format!(
        "board reply {need} {stage} rc={} {}",
        out.code,
        crate::pump::board::last_line(&out.text, 80)
    ));
    out.code == 0
}

#[cfg(test)]
#[path = "record_test.rs"]
mod tests;
