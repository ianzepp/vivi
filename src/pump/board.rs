//! Need intake: candidate rows (needs with an open pump record), re-plan
//! of answered needs, and the pump's replies.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use crate::pump::host::{Ctx, shell_quote};
use crate::pump::ready::satisfied;
use crate::pump::state::{State, Status, Unit};

mod replan;
pub use replan::replan_unblocked;

/// One row of `vivi need list --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedRow {
    /// The need handle.
    pub handle: String,
    /// ISO date the need was sent.
    pub date: String,
    /// Subject line.
    pub subject: String,
    /// Marks on the need.
    pub marks: Vec<String>,
}

/// Parse the JSON array of a `need list --json` output, skipping any text
/// before the first `[`. Invalid output gives `None`.
#[must_use]
pub fn parse_rows(out: &str) -> Option<Vec<NeedRow>> {
    let start = out.find('[')?;
    let Ok(values) = serde_json::from_str::<Vec<Value>>(&out[start..]) else {
        return None;
    };
    Some(values.iter().map(row_from).collect())
}

/// Build a row from one JSON object; missing keys are empty.
fn row_from(value: &Value) -> NeedRow {
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let marks = value
        .get("marks")
        .and_then(Value::as_array)
        .map(|marks| {
            marks
                .iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    NeedRow {
        handle: text("handle"),
        date: text("date"),
        subject: text("subject"),
        marks,
    }
}

/// The board's candidate rows: the open needs that have an open pump record
/// (`vivi pump send --need <handle>`), read from the held list and from the
/// list addressed to `reply_from`. A need without an open record is never
/// planned, whatever its sender, date or marks.
#[must_use]
pub fn candidate_rows(ctx: &Ctx) -> Vec<NeedRow> {
    let Some(wanted) = crate::pump::record::open_need_handles(ctx) else {
        return Vec::new();
    };
    if wanted.is_empty() {
        return Vec::new();
    }
    let forms = [
        String::from("need list --status open --json --held"),
        format!("need list --status open --json --to {}", ctx.cfg.reply_from),
    ];
    let mut rows: Vec<NeedRow> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for args in &forms {
        let out = ctx.vivi(args, 60);
        let Some(found) = parse_rows(&out.text) else {
            ctx.log(&format!(
                "board list unreadable ({args}): {}",
                tail_note(&out.text, 150)
            ));
            continue;
        };
        for row in found.into_iter().filter(|row| wanted.contains(&row.handle)) {
            if seen.insert(row.handle.clone()) {
                rows.push(row);
            }
        }
    }
    rows
}

/// Handles of the needs addressed to the pump identity that are closed in
/// the mailspace, or `None` when the list is unreadable.
pub fn done_need_handles(ctx: &Ctx) -> Option<BTreeSet<String>> {
    let out = ctx.vivi(
        &format!(
            "need list --status done --to {} --json",
            shell_quote(&ctx.cfg.reply_from)
        ),
        60,
    );
    let rows = if out.code == 0 {
        parse_rows(&out.text)
    } else {
        None
    };
    let Some(rows) = rows else {
        ctx.log(&format!(
            "done need list unreadable: {}",
            tail_note(&out.text, 150)
        ));
        return None;
    };
    Some(
        rows.into_iter()
            .map(|row| row.handle)
            .filter(|handle| !handle.is_empty())
            .collect(),
    )
}

/// Reconcile needs closed on the board outside the pump. Unfinished units
/// of those needs are withdrawn so they are never dispatched or merged.
pub fn reconcile_done_needs(ctx: &Ctx, st: &mut State) -> Option<BTreeSet<String>> {
    let done = done_need_handles(ctx)?;
    crate::pump::record::settle_closed_needs(ctx, st, &done);
    let withdrawn = withdraw_closed_need_units(st, &done, ctx.now());
    for (need, count) in withdrawn {
        ctx.log(&format!(
            "closed need {need}: withdrew {count} unfinished unit(s)"
        ));
    }
    Some(done)
}

/// Withdraw every unfinished unit of a closed need; counts per need.
fn withdraw_closed_need_units(
    st: &mut State,
    done: &BTreeSet<String>,
    now: u64,
) -> BTreeMap<String, usize> {
    let mut withdrawn = BTreeMap::<String, usize>::new();
    for unit in st.units.values_mut() {
        if done.contains(&unit.need) && unfinished_closed_need_unit(unit) {
            unit.status = Status::Withdrawn;
            unit.since = now;
            unit.ready_at = None;
            *withdrawn.entry(unit.need.clone()).or_default() += 1;
        }
    }
    withdrawn
}

pub(crate) fn unfinished_closed_need_unit(unit: &Unit) -> bool {
    matches!(
        unit.status,
        Status::Pending
            | Status::Held
            | Status::NeedsReplan
            | Status::Inflight
            | Status::Delivered
            | Status::Merging
    )
}

/// The log line a pass writes when control stops it between two needs.
const STOP_NOTE: &str = "board: control stop requested; finishing the pass early";

/// True when the operator asked the pump to stop or shut down. A read error
/// returns false: the run loop's `exit_requested` already handles hard
/// failures between ticks.
fn stop_requested(ctx: &Ctx) -> bool {
    matches!(
        crate::pump::control::read(&ctx.home),
        Ok(control)
            if control.mode == crate::pump::control::PumpMode::Stopped
                || control.shutdown_requested
    )
}

/// `<home>/plans/<handle>.json`.
fn plan_path(ctx: &Ctx, handle: &str) -> PathBuf {
    ctx.home.join("plans").join(format!("{handle}.json"))
}

/// The last `max` characters of `text`, newlines folded to `" | "`.
fn tail_note(text: &str, max: usize) -> String {
    let skip = text.chars().count().saturating_sub(max);
    let tail: String = text.chars().skip(skip).collect();
    tail.replace('\n', " | ")
}

/// Verdicts that wait for an operator ruling before anything moves.
pub const PARKED: [&str; 3] = ["operator", "design", "hold"];

/// True when the need's thread holds a message newer than `since` (epoch
/// seconds) from anyone but the pump itself: `## <rfc3339> - <hex> [` header
/// lines set the current message time, and the following `From:` line
/// decides it. The pump's own replies, and the event letters it holds as the
/// item's holder, come from `reply_from` and never count. Any other role
/// (the operator, a CTO, a head) answering a parked verdict re-plans it.
/// A nonzero exit, or an unparseable stamp, means false.
#[must_use]
pub fn replied_after(ctx: &Ctx, handle: &str, since: u64) -> bool {
    let out = ctx.vivi(&format!("need show {handle}"), 60);
    if out.code != 0 {
        return false;
    }
    let pump = format!("From: {}@", ctx.cfg.reply_from);
    let mut when = String::new();
    for line in out.text.lines() {
        if let Some(stamp) = header_time(line) {
            stamp.clone_into(&mut when);
        } else if !when.is_empty()
            && line.starts_with("From: ")
            && !line.starts_with(&pump)
            && newer_than(&when, since)
        {
            return true;
        }
    }
    false
}

/// The RFC 3339 stamp of a `## <rfc3339> - <hex> [` thread header line.
fn header_time(line: &str) -> Option<&str> {
    let (stamp, rest) = line.strip_prefix("## ")?.split_once(' ')?;
    let (dash, rest) = rest.split_once(' ')?;
    let (hex, rest) = rest.split_once(' ')?;
    if dash != "-" || hex.is_empty() || !rest.starts_with('[') {
        return None;
    }
    hex.chars()
        .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        .then_some(stamp)
}

/// True when RFC 3339 `stamp` parses and is strictly later than `since`.
fn newer_than(stamp: &str, since: u64) -> bool {
    chrono::DateTime::parse_from_rfc3339(stamp)
        .is_ok_and(|time| u64::try_from(time.timestamp()).is_ok_and(|secs| secs > since))
}

/// Epoch seconds of a file's mtime; 0 when the clock sits before the epoch.
fn mtime_secs(meta: &fs::Metadata) -> u64 {
    let Ok(modified) = meta.modified() else {
        return 0;
    };
    u64::try_from(chrono::DateTime::<chrono::Utc>::from(modified).timestamp()).unwrap_or(0)
}

/// A parked need (verdict `operator`, `design` or `hold`, no units) whose
/// thread gained a reply from anyone but the pump after its plan was written
/// is planned again: the plan moves to `plans/superseded/`, the basename leaves
/// `plans_seen`, the need record and its `planned` reply key are dropped, and
/// the board plans the need again on its open pump record.
pub fn replan_answered(ctx: &Ctx, st: &mut State, rows: &[NeedRow]) {
    for row in rows {
        let Some(verdict) = parked_with_reply(ctx, st, &row.handle) else {
            continue;
        };
        replan::forget_plan(ctx, st, &row.handle, "superseded");
        ctx.log(&format!(
            "board: operator replied on parked need {} (was {verdict}); re-planning",
            row.handle
        ));
    }
}

/// The need's verdict when `handle` is parked, unit-less, has a plan file,
/// and its thread holds a non-pump reply newer than that plan's mtime.
fn parked_with_reply(ctx: &Ctx, st: &State, handle: &str) -> Option<String> {
    let record = st.needs.get(handle)?;
    if !PARKED.contains(&record.verdict.as_str()) || !record.units.is_empty() {
        return None;
    }
    let Ok(meta) = fs::metadata(plan_path(ctx, handle)) else {
        return None;
    };
    replied_after(ctx, handle, mtime_secs(&meta)).then(|| record.verdict.clone())
}

/// Reply once on every candidate need's open pump record that has a verdict:
/// stage `planned` always, then implementation or landing when units are
/// settled. A plan verdict that waits for an operator ruling is posted on the
/// need's own thread instead, where the operator answers it. Returns whether
/// any reply was sent.
pub fn post_replies(ctx: &Ctx, st: &mut State, rows: &[NeedRow]) -> bool {
    let mut said = false;
    for row in rows {
        let Some((verdict, reason, units, revision)) = st.needs.get(&row.handle).map(|record| {
            (
                record.verdict.clone(),
                record.reason.clone(),
                record.units.clone(),
                record.revision,
            )
        }) else {
            continue;
        };
        if reply_stages(ctx, st, &row.handle, &verdict, &reason, &units, revision) {
            said = true;
        }
        if stop_requested(ctx) {
            ctx.log(STOP_NOTE);
            return said;
        }
    }
    said
}

/// Send the pending stages of one need; true when any reply succeeded.
fn reply_stages(
    ctx: &Ctx,
    st: &mut State,
    handle: &str,
    verdict: &str,
    reason: &str,
    units: &[String],
    revision: u64,
) -> bool {
    let mut said = false;
    let mut stages = vec![String::from("planned")];
    if revision > 1 {
        stages.push(format!("revised-{revision}"));
    }
    if !units.is_empty() && units.iter().all(|uid| satisfied(st, uid, 0)) {
        stages.push(String::from("merged"));
    }
    for stage in stages {
        let key = reply_dedup_key(handle, &stage);
        if st.board.replied.iter().any(|sent| sent == &key) {
            continue;
        }
        let Some(mut body) = reply_body(st, &stage, verdict, reason, units, revision) else {
            continue;
        };
        if stage == "merged"
            && let Some(line) = merged_timing(ctx, handle)
        {
            body.push('\n');
            body.push_str(&line);
        }
        let sent = if crate::pump::record::awaits_ruling(&stage, verdict) {
            crate::pump::record::send_need_reply(ctx, handle, &stage, &body)
        } else {
            crate::pump::record::send_reply(ctx, handle, &stage, &body)
        };
        if sent {
            st.board.replied.push(key);
            said = true;
        }
    }
    said
}

fn reply_dedup_key(handle: &str, stage: &str) -> String {
    format!("{handle}:{stage}")
}

/// The timing summary for a need's `merged` reply, read back through the CLI
/// so the pump consumes its own posted records like a synced second member.
/// Any failure (nonzero exit, unreadable JSON, no `attempts` array) is `None`:
/// the reply is never blocked or failed by this.
fn merged_timing(ctx: &Ctx, handle: &str) -> Option<String> {
    let out = ctx.vivi(&format!("need attempts {handle} --json"), 60);
    if out.code != 0 {
        return None;
    }
    let start = out.text.find('{')?;
    let Ok(value) = serde_json::from_str::<Value>(&out.text[start..]) else {
        return None;
    };
    let mut attempts: Vec<crate::pump::attempt_read::AttemptFields> = Vec::new();
    for attempt in value.get("attempts")?.as_array()? {
        if let Ok(fields) =
            serde_json::from_value::<crate::pump::attempt_read::AttemptFields>(attempt.clone())
        {
            attempts.push(fields);
        }
    }
    let opened = value.get("opened").and_then(Value::as_u64);
    Some(crate::pump::attempt_read::timing_summary(
        opened,
        &attempts,
        ctx.now(),
    ))
}

/// The reply body for one stage, or `None` when the stage must wait for units
/// to settle.
fn reply_body(
    st: &State,
    stage: &str,
    verdict: &str,
    reason: &str,
    units: &[String],
    revision: u64,
) -> Option<String> {
    if stage == "planned" {
        let list = unit_list(st, units, false);
        return Some(format!(
            "pump: planned. verdict: {verdict}; revision {revision}; {} unit(s).\n{reason}\n{list}",
            units.len()
        ));
    }
    if stage.starts_with("revised-") {
        let list = unit_list(st, units, false);
        return Some(format!(
            "pump: revised to r{revision}. {} unit(s).\n{list}",
            units.len()
        ));
    }
    if stage == "merged" && !units.is_empty() && units.iter().all(|uid| satisfied(st, uid, 0)) {
        let list = unit_list(st, units, true);
        return Some(format!(
            "pump: every unit of this need is merged into its target integration branch (r{revision}).\n{list}\nNot reviewed. Not on main."
        ));
    }
    None
}

/// `- <uid> <title>` or, with `commits`, `- <uid> <status> <commit>`.
fn unit_list(st: &State, units: &[String], commits: bool) -> String {
    units
        .iter()
        .filter_map(|uid| {
            st.units.get(uid).map(|unit| {
                if commits {
                    let commit: String = unit
                        .commit
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(10)
                        .collect();
                    format!(
                        "- {uid} [{}] {} {commit}",
                        unit.target_id(),
                        unit.status.as_str()
                    )
                } else {
                    format!("- {uid} [{}] {}", unit.target_id(), unit.title)
                }
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The last line of `text` cut to `max` chars, empty when the output is blank.
pub(crate) fn last_line(text: &str, max: usize) -> String {
    text.trim()
        .lines()
        .last()
        .unwrap_or("")
        .chars()
        .take(max)
        .collect()
}

/// One board pass: pull, re-plan answered needs, and push replies. The
/// interval starts when the pass completes.
pub fn board(ctx: &Ctx, st: &mut State) {
    if ctx.now().saturating_sub(st.board.at) < ctx.cfg.board.every {
        return;
    }
    if ctx.cfg.board.pull {
        let out = ctx.vivi("sync pull", 120);
        ctx.log(&format!(
            "sync pull rc={} {}",
            out.code,
            last_line(&out.text, 150)
        ));
    }
    let rows = candidate_rows(ctx);
    replan_answered(ctx, st, &rows);
    replan_unblocked(ctx, st, &rows);
    let said = post_replies(ctx, st, &rows);
    let handles: Vec<String> = rows.iter().map(|row| row.handle.clone()).collect();
    crate::pump::record::settle_gave_up(ctx, st, &handles);
    if std::mem::take(&mut st.board.sync_pending) || said {
        spawn_sync_push(ctx);
    }
    st.board.at = ctx.now();
}

/// Start the detached drain push that ships the replies just posted.
pub(crate) fn spawn_sync_push(ctx: &Ctx) {
    let project = shell_quote(&ctx.project.to_string_lossy());
    let core = format!(
        "{} sync push --since {} --drain --project {project}",
        ctx.vivi, ctx.cfg.board.push_since
    );
    let cmd = if ctx.shell_prefix.is_empty() {
        core
    } else {
        format!("{}; {core}", ctx.shell_prefix)
    };
    ctx.host
        .spawn(&cmd, None, None, &ctx.home.join("sync-push.log"));
    ctx.log("sync push started (detached, drain)");
}
