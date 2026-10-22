//! The pump's side of pump records.
//!
//! One dispatch of a need is one pump record (see
//! [`crate::mailspace::PumpRecord`]). The pump plans exactly the needs that
//! have an open record, posts its progress as replies on that record's
//! thread, and closes the record with an outcome word when the dispatch ends:
//!
//! - `landed`: the need closed and every unit of it was satisfied;
//! - `gave-up`: the planner, the need audit or the units parked it for good;
//! - `withdrawn`: the need was withdrawn, or closed without finishing;
//! - `superseded`: an operator replan threw the plan away (a fresh record is
//!   opened so the replanned need is planned again).
//!
//! An operator's `withdraw` or `replan` closes its record through
//! [`settle_action`], after the pump has saved the state change
//! ([`crate::pump::action`] owns the exactly-once rules).
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
use crate::pump::state::{ActionMark, State, Status};

/// The dispatch ended with the need closed and all its work merged.
pub const LANDED: &str = "landed";
/// The pump parked the need and will not move it without an operator.
pub const GAVE_UP: &str = "gave-up";
/// The need was withdrawn, or closed before its work finished.
pub const WITHDRAWN: &str = "withdrawn";
/// An operator replan replaced the plan this dispatch carried.
pub const SUPERSEDED: &str = "superseded";

/// Recursion cap when following `after` and `replaced_by` chains.
const DEPTH: u32 = 8;

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

/// A plan reply whose verdict waits for an operator ruling: the operator
/// answers it on the need's own thread, so it is posted there.
pub(crate) fn awaits_ruling(stage: &str, verdict: &str) -> bool {
    (stage == "planned" || stage.starts_with("revised-"))
        && crate::pump::board::PARKED.contains(&verdict)
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
/// `target` is a pump record or a need handle.
pub fn reply_on(ctx: &Ctx, target: &str, need: &str, stage: &str, body: &str) -> bool {
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
    let reply_to = ctx.cfg.reply_to();
    if !reply_to.is_empty() {
        args.push_str(&format!(" --to {}", shell_quote(&reply_to)));
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

/// Settle the record effect an applied operator action still owes: close the
/// record it was posted on with `mark.owes` (`withdrawn` or `superseded`),
/// and for `superseded` open a fresh record so the board plans the need
/// again and each record carries exactly one plan.
///
/// Every step checks before it acts, so replaying it after a tick that died
/// part-way is safe: the record is closed only while it is still open, and a
/// fresh one is opened only while the need has none. Returns whether the debt
/// is settled; `false` means try again next tick. A failed send of the fresh
/// record is raised as an operator exception and the debt is dropped, because
/// the need would otherwise never be replanned and the operator must
/// `vivi pump send` it.
pub fn settle_action(ctx: &Ctx, mailspace: &Mailspace, mark: &ActionMark) -> bool {
    let need = mark.need.as_str();
    let open = match mailspace.pump_list(Some(need), Some(PumpState::Open)) {
        Ok(open) => open,
        Err(error) => {
            ctx.log(&format!("pump record lookup for {need} failed: {error}"));
            return false;
        }
    };
    if open.iter().any(|record| record.handle == mark.record)
        && close_record(ctx, mailspace, &mark.record, need, &mark.owes, &mark.detail).is_none()
    {
        return false;
    }
    if mark.owes != SUPERSEDED {
        return true;
    }
    if open.iter().any(|record| record.handle != mark.record) {
        return true;
    }
    let body = format!("Replan of need {need}: the previous plan was superseded.");
    match mailspace.pump_send(None, need, Some(&body)) {
        Ok(record) => ctx.log(&format!(
            "pump record {} opened for the replan of need {need}",
            record.handle
        )),
        Err(error) => ctx.exception(
            "replan-record-failed",
            need,
            &format!(
                "no pump record was opened for the replan, so the board will not plan it: \
                 {error}; send one with `vivi pump send --need {need}`"
            ),
        ),
    }
    true
}

/// A revision of `need` is starting (its thread moved after the plan): make
/// sure the need has an open pump record to carry it. A need whose record was
/// closed (`landed`, `gave-up`) and then amended is a new dispatch, so it gets
/// a new record; one that still has its record keeps it. A failure is logged:
/// the revision itself does not depend on the record.
pub fn ensure_open(ctx: &Ctx, need: &str) {
    let Some(mailspace) = open_mailspace(ctx) else {
        return;
    };
    match mailspace.pump_list(Some(need), Some(PumpState::Open)) {
        Ok(open) if !open.is_empty() => return,
        Ok(_) => {}
        Err(error) => {
            ctx.log(&format!("pump record lookup for {need} failed: {error}"));
            return;
        }
    }
    let body = format!("Amendment of need {need}: its thread changed after the plan.");
    match mailspace.pump_send(None, need, Some(&body)) {
        Ok(record) => ctx.log(&format!(
            "pump record {} opened for the amendment of need {need}",
            record.handle
        )),
        Err(error) => ctx.log(&format!(
            "no pump record opened for the amendment of need {need}: {error}"
        )),
    }
}

/// Close the open records of needs that were closed on the board. A need
/// whose work all merged `landed`; one closed with work unfinished, or before
/// it was planned, is `withdrawn`.
pub fn settle_closed_needs(ctx: &Ctx, st: &State, done: &BTreeSet<String>) {
    if done.is_empty() {
        return;
    }
    let Some(mailspace) = open_mailspace(ctx) else {
        return;
    };
    let open = match mailspace.pump_list(None, Some(PumpState::Open)) {
        Ok(open) => open,
        Err(error) => {
            ctx.log(&format!("pump record list failed: {error}"));
            return;
        }
    };
    for record in open.iter().filter(|record| done.contains(&record.need)) {
        let (outcome, detail) = closed_need_outcome(st, &record.need);
        close_record(
            ctx,
            &mailspace,
            &record.handle,
            &record.need,
            outcome,
            &detail,
        );
    }
}

/// How a dispatch ended when its need was closed: `landed` when the pump's
/// own account says the need is complete, else `withdrawn` with the reason.
#[must_use]
pub fn closed_need_outcome(st: &State, need: &str) -> (&'static str, String) {
    if crate::pump::report::need_complete(st, need) {
        return (LANDED, String::new());
    }
    let unfinished = st
        .units
        .iter()
        .filter(|(_, unit)| unit.need == need && unit.status != Status::Withdrawn)
        .filter(|(uid, _)| !crate::pump::ready::satisfied(st, uid, 0))
        .count();
    let detail = if st.needs.contains_key(need) {
        format!("need closed outside the pump with {unfinished} unit(s) unfinished")
    } else {
        String::from("need closed before the pump planned it")
    };
    (WITHDRAWN, detail)
}

/// Close the open record of every listed need that the pump has given up on.
/// Idempotent: a closed record leaves the intake, so it is not seen again.
///
/// A record the tick thread has not adopted yet is left alone: when the pump
/// last ran the need under another record, the newer one is a fresh dispatch
/// (an operator retry), and this pass's state still carries the old
/// dispatch's parked planner and stuck units. The tick adopts it, clears
/// that state and requeues the stuck units first; a later pass judges it.
pub fn settle_gave_up(ctx: &Ctx, st: &State, needs: &[String]) {
    let reasons: Vec<(&String, String)> = needs
        .iter()
        .filter_map(|need| Some((need, gave_up_reason(ctx, st, need)?)))
        .collect();
    if reasons.is_empty() {
        return;
    }
    let Some(mailspace) = open_mailspace(ctx) else {
        return;
    };
    for (need, reason) in reasons {
        let open = match mailspace.pump_list(Some(need), Some(PumpState::Open)) {
            Ok(open) => open,
            Err(error) => {
                ctx.log(&format!("pump record lookup for {need} failed: {error}"));
                continue;
            }
        };
        let Some(record) = open.into_iter().next() else {
            continue;
        };
        if st
            .board
            .records
            .get(need)
            .is_some_and(|seen| seen != &record.handle)
        {
            ctx.log(&format!(
                "pump record {} for need {need} is a new dispatch; not judged until the pump adopts it",
                record.handle
            ));
            continue;
        }
        close_record(ctx, &mailspace, &record.handle, need, GAVE_UP, &reason);
    }
}

/// Why the pump will not move `need` any further without an operator, or
/// `None` while it is still moving, waiting on a planner, or complete.
#[must_use]
pub fn gave_up_reason(ctx: &Ctx, st: &State, need: &str) -> Option<String> {
    if st.board.planner_failed.iter().any(|failed| failed == need) {
        return Some(String::from(
            "the planner exited three times without writing a plan",
        ));
    }
    if let Some(reason) = audit_hold(ctx, st, need) {
        return Some(format!("the need audit holds this need: {reason}"));
    }
    let record = st.needs.get(need)?;
    if record.amendment_pending
        || st.board.planning.contains_key(need)
        || st.board.revision_planning.contains_key(need)
    {
        return None;
    }
    let stuck = record.units.iter().find(|uid| {
        st.units
            .get(*uid)
            .is_some_and(|u| u.status == Status::Stuck)
    })?;
    let moving = record
        .units
        .iter()
        .any(|uid| !crate::pump::ready::satisfied(st, uid, 0) && can_advance(st, uid, 0));
    (!moving).then(|| format!("unit {stuck} is stuck and no other unit can advance"))
}

/// The reason a need audit hold stands on `need` for any target.
fn audit_hold(ctx: &Ctx, st: &State, need: &str) -> Option<String> {
    ctx.cfg.target_configs().keys().find_map(|target| {
        match crate::pump::need_verdict::state(st, target, need) {
            Ok(verdict) => verdict.hold.map(|hold| hold.reason),
            Err(_) => None,
        }
    })
}

/// Whether a unit can still make progress: it is running, or waiting only on
/// units that can. Stuck, replan-parked and withdrawn units cannot.
fn can_advance(st: &State, uid: &str, depth: u32) -> bool {
    let Some(unit) = st.units.get(uid) else {
        return true;
    };
    if depth > DEPTH {
        return true;
    }
    match unit.status {
        Status::Merged | Status::NoDefect => true,
        Status::Stuck | Status::NeedsReplan | Status::Withdrawn => false,
        Status::Replaced => {
            !unit.replaced_by.is_empty()
                && unit
                    .replaced_by
                    .iter()
                    .all(|next| can_advance(st, next, depth + 1))
        }
        Status::Pending | Status::Held => unit
            .after
            .iter()
            .all(|before| can_advance(st, before, depth + 1)),
        Status::Inflight
        | Status::Delivered
        | Status::Merging
        | Status::Quarantined
        | Status::Splitting
        | Status::Other(_) => true,
    }
}

#[cfg(test)]
#[path = "record_test.rs"]
mod tests;
