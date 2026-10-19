//! Need intake: candidate rows, approval marks, planner expiry and launch.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use crate::pump::collect::spawn_planner;
use crate::pump::host::{Ctx, exit_receipt_path, shell_quote};
use crate::pump::ready::satisfied;
use crate::pump::state::State;

/// One row of `vivi need list --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedRow {
    /// The need handle.
    pub handle: String,
    /// ISO date the need was sent.
    pub date: String,
    /// Subject line.
    pub subject: String,
    /// Marks on the need; the board's approval mark makes it plannable.
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
    let marks = value
        .get("marks")
        .and_then(Value::as_array)
        .map(|marks| {
            marks
                .iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    NeedRow {
        handle: value
            .get("handle")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        date: value
            .get("date")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        subject: value
            .get("subject")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        marks,
    }
}

/// Open needs, from any sender, that carry the approval mark:
/// held needs first, then needs addressed to the reply identity; duplicate
/// handles keep the first row. An empty mark means no marked rows at all.
#[must_use]
pub fn marked_rows(ctx: &Ctx) -> Vec<NeedRow> {
    let mut found: Vec<NeedRow> = Vec::new();
    let mark = ctx.cfg.board.mark.as_str();
    if mark.is_empty() {
        return found;
    }
    let forms = [
        String::from("need list --status open --json --held"),
        format!("need list --status open --json --to {}", ctx.cfg.reply_from),
    ];
    for args in &forms {
        let out = ctx.vivi(args, 60);
        let Some(rows) = parse_rows(&out.text) else {
            ctx.log(&format!(
                "marked list unreadable ({args}): {}",
                tail_note(&out.text, 150)
            ));
            continue;
        };
        found.extend(
            rows.into_iter()
                .filter(|row| row.marks.iter().any(|m| m.as_str() == mark)),
        );
    }
    let mut unique: Vec<NeedRow> = Vec::new();
    for row in found {
        if !unique.iter().any(|seen| seen.handle == row.handle) {
            unique.push(row);
        }
    }
    unique
}

/// The board's candidate rows: every configured sender's open needs
/// addressed to `reply_from`, then marked rows not already present; plus the
/// set of marked (approved) handles. A need addressed to another recipient is
/// not the pump's work, and a thread reply never becomes one.
#[must_use]
pub fn candidate_rows(ctx: &Ctx) -> (Vec<NeedRow>, BTreeSet<String>) {
    let mut rows: Vec<NeedRow> = Vec::new();
    for sender in &ctx.cfg.board.from {
        let out = ctx.vivi(
            &format!(
                "need list --from {sender} --to {} --status open --json",
                ctx.cfg.reply_from
            ),
            60,
        );
        match parse_rows(&out.text) {
            Some(found) => rows.extend(found),
            None => ctx.log(&format!(
                "board list unreadable: {}",
                tail_note(&out.text, 150)
            )),
        }
    }
    let marked = marked_rows(ctx);
    let approved: BTreeSet<String> = marked.iter().map(|row| row.handle.clone()).collect();
    for row in marked {
        if !rows.iter().any(|seen| seen.handle == row.handle) {
            rows.push(row);
        }
    }
    (rows, approved)
}

/// Whitespace-separated handles of `<home>/skip.txt`; missing file means
/// nothing is skipped.
#[must_use]
pub fn read_skip(ctx: &Ctx) -> BTreeSet<String> {
    match fs::read_to_string(ctx.home.join("skip.txt")) {
        Ok(text) => text.split_whitespace().map(String::from).collect(),
        Err(_) => BTreeSet::new(),
    }
}

/// Drop planner entries whose plan file exists (done), whose planner exited
/// without a plan, or that ran longer than `plan_timeout`. A planner exit
/// receipt (`.rc`, written by the spawn reaper) frees the slot at once and
/// schedules a backoff relaunch; a timed-out planner without a receipt still
/// raises `plan-no-answer`. A landed plan clears the handle's planner
/// bookkeeping so a later re-plan starts clean.
pub fn expire_planning(ctx: &Ctx, st: &mut State) {
    let timeout = ctx.cfg.board.plan_timeout;
    let mins = timeout / 60;
    let now = ctx.now();
    let expired: Vec<String> = st
        .board
        .planning
        .iter()
        .filter(|(handle, at)| {
            plan_path(ctx, handle).exists()
                || exit_receipt_path(&out_path(ctx, handle)).exists()
                || now.saturating_sub(**at) > timeout
        })
        .map(|(handle, _)| handle.clone())
        .collect();
    for handle in expired {
        if plan_path(ctx, &handle).exists() {
            st.board.planner_pids.remove(&handle);
            st.board.planner_fails.remove(&handle);
            st.board.planner_retry_at.remove(&handle);
        } else if exit_receipt_path(&out_path(ctx, &handle)).exists() {
            fail_exited_planner(ctx, st, &handle);
        } else {
            ctx.exception(
                "plan-no-answer",
                &handle,
                &format!("the planner wrote no plan in {mins} minutes"),
            );
        }
        st.board.planning.remove(&handle);
    }
}

/// Consecutive planner crashes tolerated before the need parks, and the base
/// seconds of the exponential relaunch delay.
const PLANNER_TRIES: u32 = 3;
const PLANNER_BACKOFF: u64 = 60;

/// `<home>/board/<handle>.out`, the planner's captured output.
fn out_path(ctx: &Ctx, handle: &str) -> PathBuf {
    ctx.home.join("board").join(format!("{handle}.out"))
}

/// Log, flag and free one planner that exited without writing its plan: the
/// slot opens this pass, a relaunch waits [`PLANNER_BACKOFF`] doubled per
/// prior failure, and after [`PLANNER_TRIES`] failures the need parks as
/// `plan-failed`.
fn fail_exited_planner(ctx: &Ctx, st: &mut State, handle: &str) {
    let code = match fs::read_to_string(exit_receipt_path(&out_path(ctx, handle))) {
        Ok(text) => text.trim().parse::<i32>().unwrap_or(-1),
        Err(_) => -1,
    };
    let text = fs::read_to_string(out_path(ctx, handle)).unwrap_or_default();
    let tail = tail_note(&text, 200);
    ctx.log(&format!("board: planner {handle} exited rc={code}: {tail}"));
    ctx.exception(
        "plan-crashed",
        handle,
        &format!("planner exited rc={code} without a plan: {tail}"),
    );
    st.board.planner_pids.remove(handle);
    let fails = st.board.planner_fails.entry(handle.to_owned()).or_insert(0);
    *fails += 1;
    let fails = *fails;
    if fails < PLANNER_TRIES {
        let delay = PLANNER_BACKOFF.saturating_mul(1u64 << fails.saturating_sub(1).min(63));
        st.board
            .planner_retry_at
            .insert(handle.to_owned(), ctx.now() + delay);
        return;
    }
    st.board.planner_failed.push(handle.to_owned());
    st.board.planner_retry_at.remove(handle);
    ctx.exception(
        "plan-failed",
        handle,
        "planner exited three times without writing a plan",
    );
}

/// Plan the new needs among `rows`: skip old, skipped,
/// known, already-planning or already-planned handles, respect the
/// concurrent-planner cap, then write the need file, spawn the planner and
/// record the planning entry. `replan` bypasses the date cutoff; launched
/// handles leave it and the mirror `st.board.replan` is re-sorted.
pub fn launch_planners(
    ctx: &Ctx,
    st: &mut State,
    rows: &[NeedRow],
    approved: &BTreeSet<String>,
    replan: &mut BTreeSet<String>,
) {
    let skip = read_skip(ctx);
    for row in rows {
        let handle = row.handle.as_str();
        let old = row.date.as_str() < ctx.cfg.board.since.as_str();
        let parked = st.board.planner_failed.iter().any(|seen| seen == handle);
        let waiting = st
            .board
            .planner_retry_at
            .get(handle)
            .is_some_and(|at| *at > ctx.now());
        if (old && !replan.contains(handle) && !approved.contains(handle))
            || skip.contains(handle)
            || st.needs.contains_key(handle)
            || st.board.planning.contains_key(handle)
            || parked
            || waiting
        {
            continue;
        }
        if plan_path(ctx, handle).exists()
            || st.board.planning.len()
                >= usize::try_from(ctx.cfg.board.planners_max).unwrap_or(usize::MAX)
        {
            continue;
        }
        launch_one(ctx, st, row, replan);
        if stop_requested(ctx) {
            ctx.log(STOP_NOTE);
            return;
        }
    }
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

/// Write the need file, spawn the planner for one row, and record it.
fn launch_one(ctx: &Ctx, st: &mut State, row: &NeedRow, replan: &mut BTreeSet<String>) {
    let handle = row.handle.as_str();
    let thread = ctx.vivi(&format!("need show {handle}"), 60).text;
    let thread: String = thread.chars().take(40_000).collect();
    let needs = ctx.home.join("needs");
    drop(fs::create_dir_all(&needs));
    drop(write_json(
        &needs.join(format!("{handle}.json")),
        &NeedFile {
            need: handle,
            subject: row.subject.as_str(),
            triage: "",
            thread: thread.as_str(),
        },
    ));
    let home = ctx.home.to_string_lossy();
    let targets = target_catalog(ctx);
    let prompt = format!(
        "Plan one need, as your system prompt describes. Its thread is in {home}/needs/{handle}.json. \
         Available repository targets (id and workspace-relative path): {targets}. Every unit must name \
         exactly one target from this list in its `target` field, and its `write_scope` must be relative \
         to that repository. Do not make an `after` dependency cross targets: the pump cannot pass an \
         unpublished branch from one repository into another; park the need if that handoff is required. \
         Write {home}/plans/{handle}.json. Your final message is one line: handle, \
         verdict, number of units."
    );
    let out = out_path(ctx, handle);
    drop(fs::remove_file(exit_receipt_path(&out)));
    drop(fs::File::create(&out));
    match spawn_planner(ctx, "board", handle, &prompt) {
        Some(pid) => {
            st.board.planner_pids.insert(handle.to_owned(), pid);
        }
        None => {
            st.board.planner_pids.remove(handle);
        }
    }
    st.board.planning.insert(handle.to_owned(), ctx.now());
    if replan.remove(handle) {
        st.board.replan = replan.iter().cloned().collect();
    }
    let subject: String = row.subject.chars().take(70).collect();
    ctx.log(&format!("board: planning new need {handle} {subject}"));
}

/// JSON catalog of named execution targets for planner assignment.
fn target_catalog(ctx: &Ctx) -> String {
    let targets: Vec<_> = ctx
        .cfg
        .target_configs()
        .into_iter()
        .map(|(id, target)| serde_json::json!({"id": id, "path": target.path}))
        .collect();
    serde_json::to_string(&targets).unwrap_or_else(|_| String::from("[]"))
}

/// `<home>/plans/<handle>.json`.
fn plan_path(ctx: &Ctx, handle: &str) -> PathBuf {
    ctx.home.join("plans").join(format!("{handle}.json"))
}

/// The need file a planner reads.
#[derive(Serialize)]
struct NeedFile<'a> {
    need: &'a str,
    subject: &'a str,
    triage: &'a str,
    thread: &'a str,
}

/// Write `value` as JSON with a one-space indent, like
/// `json.dump(..., indent=1)`.
fn write_json<T: Serialize>(path: &PathBuf, value: &T) -> std::io::Result<()> {
    let file = fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut writer, formatter);
    value.serialize(&mut serializer)?;
    writer.flush()
}

/// The last `max` characters of `text`, newlines folded to `" | "`.
fn tail_note(text: &str, max: usize) -> String {
    let skip = text.chars().count().saturating_sub(max);
    let tail: String = text.chars().skip(skip).collect();
    tail.replace('\n', " | ")
}

/// Verdicts that wait for an operator ruling before anything moves.
pub(crate) const PARKED: [&str; 3] = ["operator", "design", "hold"];

/// True when the need's thread holds an operator message newer than `since`
/// (epoch seconds): `## <rfc3339> - <hex> [` header lines set the current
/// message time, and a following `From: <cfg.from>@` line decides it.
/// A nonzero exit, or an unparseable stamp, means false.
#[must_use]
pub fn operator_replied_after(ctx: &Ctx, handle: &str, since: u64) -> bool {
    let out = ctx.vivi(&format!("need show {handle}"), 60);
    if out.code != 0 {
        return false;
    }
    let sender = format!("From: {}@", ctx.cfg.from);
    let mut when = String::new();
    for line in out.text.lines() {
        if let Some(stamp) = header_time(line) {
            stamp.clone_into(&mut when);
        } else if !when.is_empty() && line.starts_with(&sender) && newer_than(&when, since) {
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
        .is_ok_and(|time| u64::try_from(time.timestamp()).unwrap_or(0) > since)
}

/// Epoch seconds of a file's mtime; 0 when the clock sits before the epoch.
fn mtime_secs(meta: &fs::Metadata) -> u64 {
    let Ok(modified) = meta.modified() else {
        return 0;
    };
    u64::try_from(chrono::DateTime::<chrono::Utc>::from(modified).timestamp()).unwrap_or(0)
}

/// A parked need (verdict `operator`, `design` or `hold`, no units) whose
/// thread gained an operator reply after its plan was written is planned
/// again: the plan moves to `plans/superseded/`, the basename leaves
/// `plans_seen`, the need record and its `planned` reply key are dropped, and
/// the handle joins the replan set, which bypasses the since cutoff.
/// Returns the replan set, mirrored sorted in
/// `st.board.replan`.
pub fn replan_answered(ctx: &Ctx, st: &mut State, rows: &[NeedRow]) -> BTreeSet<String> {
    let mut replan: BTreeSet<String> = st.board.replan.iter().cloned().collect();
    for row in rows {
        let Some(verdict) = parked_with_reply(ctx, st, &row.handle) else {
            continue;
        };
        let superseded = ctx.home.join("plans").join("superseded");
        drop(fs::create_dir_all(&superseded));
        drop(fs::rename(
            plan_path(ctx, &row.handle),
            superseded.join(format!("{}.{}.json", row.handle, ctx.now())),
        ));
        let json = format!("{}.json", row.handle);
        st.plans_seen.retain(|seen| seen != &json);
        st.needs.remove(&row.handle);
        let key = format!("{}:planned", row.handle);
        st.board.replied.retain(|sent| sent != &key);
        replan.insert(row.handle.clone());
        ctx.log(&format!(
            "board: operator replied on parked need {} (was {verdict}); re-planning",
            row.handle
        ));
    }
    st.board.replan = replan.iter().cloned().collect();
    replan
}

/// The need's verdict when `handle` is parked, unit-less, has a plan file,
/// and its thread holds an operator reply newer than that plan's mtime.
fn parked_with_reply(ctx: &Ctx, st: &State, handle: &str) -> Option<String> {
    let record = st.needs.get(handle)?;
    if !PARKED.contains(&record.verdict.as_str()) || !record.units.is_empty() {
        return None;
    }
    let Ok(meta) = fs::metadata(plan_path(ctx, handle)) else {
        return None;
    };
    operator_replied_after(ctx, handle, mtime_secs(&meta)).then(|| record.verdict.clone())
}

/// Reply once on every candidate need's thread that has a verdict: stage
/// `planned` always, stage `merged` when every unit is satisfied.
/// Returns whether any reply was sent.
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
    stages.push(String::from("merged"));
    for stage in stages {
        let key = format!("{handle}:{stage}");
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
        if send_reply(ctx, handle, &stage, &body) {
            st.board.replied.push(key);
            said = true;
        }
    }
    said
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

/// The reply body for one stage, or `None` when the stage must wait:
/// `merged` waits until the need has units and every one is satisfied.
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
    if !units.is_empty() && units.iter().all(|uid| satisfied(st, uid, 0)) {
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

/// Write the body to `<home>/board/reply-<h>-<stage>.txt` and run `mail
/// reply` from the reply identity; true when the reply succeeded.
pub(crate) fn send_reply(ctx: &Ctx, handle: &str, stage: &str, body: &str) -> bool {
    let dir = ctx.home.join("board");
    drop(fs::create_dir_all(&dir));
    let path = dir.join(format!("reply-{handle}-{stage}.txt"));
    if fs::write(&path, format!("{body}\n")).is_err() {
        ctx.log(&format!(
            "board reply {handle} {stage} rc=1 body file unwritable"
        ));
        return false;
    }
    let args = format!(
        "mail reply {handle} --from {} --to {} --body-file {}",
        ctx.cfg.reply_from,
        ctx.cfg.reply_to(),
        shell_quote(&path.to_string_lossy())
    );
    let out = ctx.vivi(&args, 60);
    ctx.log(&format!(
        "board reply {handle} {stage} rc={} {}",
        out.code,
        last_line(&out.text, 80)
    ));
    out.code == 0
}

/// The last line of `text` cut to
/// `max` chars, empty when the output is blank.
fn last_line(text: &str, max: usize) -> String {
    text.trim()
        .lines()
        .last()
        .unwrap_or("")
        .chars()
        .take(max)
        .collect()
}

/// One board pass: pull, expire, re-plan answered needs, plan new needs, and
/// push replies. The interval starts when the pass completes.
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
    let (rows, approved) = candidate_rows(ctx);
    expire_planning(ctx, st);
    let mut replan = replan_answered(ctx, st, &rows);
    launch_planners(ctx, st, &rows, &approved, &mut replan);
    let said = post_replies(ctx, st, &rows);
    if std::mem::take(&mut st.board.sync_pending) || said {
        spawn_sync_push(ctx);
    }
    crate::pump::amend::detect(ctx, st);
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
