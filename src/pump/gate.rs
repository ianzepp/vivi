//! Gate start, finish, gate-fix, the hold cycle and promotion.
//!
//! The gate: `start_gate`, `finish_gate`, `gatefix`, `promote` and the
//! hold cycle.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::pump::brief::gate_brief;
use crate::pump::dispatch::{parse_created_handle, slots};
use crate::pump::gatefix::{
    gatefix, plan_need, replans_left, residual_failures, send_fix_need, take_residuals,
};
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::state::{GateRecord, GateRun, Promotion, State, Status, Unit};

pub(crate) mod green;

/// The `FAILING-BEGIN` reply marker.
const BEGIN: &str = "FAILING-BEGIN";
/// The `FAILING-END` reply marker.
const END: &str = "FAILING-END";
/// The single line a gate worker replies with when the build itself broke.
const BUILD_FAILED: &str = "BUILD-FAILED";
/// Flag file that makes the next tick start a gate whatever the due rules;
/// the default target uses this name, other targets suffix `-<target>`.
const FORCE_FLAG: &str = "FORCE-GATE";
/// The `state.json` key remembering the tip whose failing set was last
/// reported unknown, so the line is logged once per tip.
const UNKNOWN_KEY: &str = "promote_unknown_logged";
/// Flag file that holds the merge queue until a clean gate.
const HOLD_FLAG: &str = "HOLD-MERGE";
/// The pool name an external gate run is recorded under.
const EXTERNAL_POOL: &str = "external";
/// The `state.json` key holding the epoch seconds a due gate began waiting
/// for an external result.
const WAIT_KEY: &str = "external_wait_since";
/// The payload the hold cycle writes into `FORCE-GATE`.
const FORCE_TEXT: &str = "after fixes\n";

/// Read the failing-test names from a gate reply.
///
/// The last `FAILING-BEGIN ... FAILING-END` block that is usable supplies the
/// names: an entirely empty block means zero failures, and `<`-prefixed
/// template lines are skipped. With no usable block and no `BUILD-FAILED` in
/// the text, the fallback reads `<crate> <path>` pairs (crate at least two
/// `[a-z0-9_-]` characters, path two or more `::`-joined segments), ignoring
/// surrounding punctuation, deduplicated and sorted. `Some(["BUILD-FAILED"])`
/// comes from a block holding exactly that line; `None` means unreadable.
#[must_use]
pub fn parse_failing(text: &str) -> Option<Vec<String>> {
    if let Some(names) = last_usable_block(text) {
        return Some(names);
    }
    if text.contains(BUILD_FAILED) {
        return None;
    }
    let pairs = crate_path_pairs(text);
    if pairs.is_empty() { None } else { Some(pairs) }
}

/// Every `FAILING-BEGIN ... FAILING-END` block body, in order: the port of
/// `FAILING-BEGIN\s*\n(.*?)\n?\s*FAILING-END` (dotall, non-greedy). A marker
/// followed by no newline starts no block; a `BEGIN` with no later `END` ends
/// the scan.
fn marker_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(BEGIN) {
        let after = &rest[at + BEGIN.len()..];
        let ws = after.len() - after.trim_start().len();
        if !after[..ws].contains('\n') {
            rest = after;
            continue;
        }
        let body = &after[ws..];
        let Some(end) = body.find(END) else {
            break;
        };
        blocks.push(body[..end].to_owned());
        rest = &body[end + END.len()..];
    }
    blocks
}

/// The last block with a usable line — non-empty and not a `<template>` — or
/// an entirely empty block, which yields the empty name list.
fn last_usable_block(text: &str) -> Option<Vec<String>> {
    for block in marker_blocks(text).iter().rev() {
        let lines: Vec<String> = block
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('<'))
            .map(String::from)
            .collect();
        if !lines.is_empty() || block.trim().is_empty() {
            return Some(lines);
        }
    }
    None
}

/// `true` for path characters: `[A-Za-z0-9_]`.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `true` for crate characters: `[a-z0-9_-]`.
fn is_crate_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'
}

/// The fallback: every `<crate> <path>` pair where the crate
/// starts on a word boundary and is at least two `[a-z0-9_-]` characters, the
/// path is two or more `::`-joined `[A-Za-z0-9_]+` segments, and punctuation
/// around the tokens (`\b` on both ends) does not count. Deduplicated and
/// sorted as `"<crate> <path>"`.
fn crate_path_pairs(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found = BTreeSet::new();
    for i in 0..bytes.len() {
        if !bytes[i].is_ascii_lowercase() || (i > 0 && is_word_byte(bytes[i - 1])) {
            continue;
        }
        let Some(space) = bytes[i..].iter().position(|b| !is_crate_byte(*b)) else {
            continue;
        };
        let crate_end = i + space;
        if space < 2 || bytes[crate_end] != b' ' {
            continue;
        }
        let Some(path_end) = parse_path(bytes, crate_end + 1) else {
            continue;
        };
        if path_end < bytes.len() && is_word_byte(bytes[path_end]) {
            continue;
        }
        found.insert(format!(
            "{} {}",
            &text[i..crate_end],
            &text[crate_end + 1..path_end]
        ));
    }
    found.into_iter().collect()
}

/// Longest `(?:[A-Za-z0-9_]+::)+[A-Za-z0-9_]+` at `start`: the end offset of
/// the path, or `None` for fewer than two `::`-joined segments. A trailing
/// `::` with no following segment backs off to the shorter path.
fn parse_path(bytes: &[u8], start: usize) -> Option<usize> {
    let mut at = start;
    let mut leading = 0;
    let mut after_last = None;
    loop {
        let seg = at;
        while at < bytes.len() && is_word_byte(bytes[at]) {
            at += 1;
        }
        if at == seg {
            break;
        }
        if bytes[at..].starts_with(b"::") {
            after_last = Some(at);
            at += 2;
            leading += 1;
        } else {
            return if leading >= 1 { Some(at) } else { None };
        }
    }
    if leading >= 2 { after_last } else { None }
}

/// Take the gate results an external runner wrote, oldest
/// first, as ordinary gates.
///
/// Does nothing unless `[pump.gate] results_dir` is set. Each `<tip>.json` in
/// it is moved to `done/` and finished like a worker's reply: `last_gate`
/// becomes an `external` run at that tip, the wait marker is cleared, and the
/// names (or a `BUILD-FAILED` line plus the tail) go through [`finish_gate`].
/// A file that does not parse is left where it is. A result with neither
/// names nor `build_failed` is finished with an empty reply, which raises
/// `gate-unreadable`.
pub fn external_gates(ctx: &Ctx, st: &mut State) {
    let Some(dir) = ctx.cfg.results_dir(&ctx.project) else {
        return;
    };
    for (path, result) in pending_results(ctx, &dir) {
        let done = dir.join("done");
        let moved = fs::create_dir_all(&done)
            .and_then(|()| fs::rename(&path, done.join(path.file_name().unwrap_or_default())));
        if let Err(err) = moved {
            ctx.log(&format!(
                "external gate result move failed at {}: {err}",
                path.display()
            ));
            continue;
        }
        let short = tip_short(&result.tip);
        if st.last_gate.task.is_some() && st.last_gate.pool.as_deref() != Some(EXTERNAL_POOL) {
            ctx.log(&format!(
                "external gate {short} taken while a pool gate is running; the pool result will follow"
            ));
        }
        st.last_gate = GateRun {
            at: ctx.now(),
            merges: st.merges,
            task: Some(String::from(EXTERNAL_POOL)),
            tip: Some(result.tip.clone()),
            pool: Some(String::from(EXTERNAL_POOL)),
        };
        st.extra.remove(WAIT_KEY);
        let host = result
            .host
            .as_deref()
            .map_or_else(String::new, |host| format!(" host={host}"));
        ctx.log(&format!(
            "external gate at {short} ({}s){host}",
            result.seconds
        ));
        finish_gate(ctx, st, &result.reply());
    }
}

/// A gate result file an external runner writes.
#[derive(Debug, serde::Deserialize)]
struct ExternalResult {
    /// The integration tip that was gated.
    tip: String,
    /// Wall seconds the run took.
    #[serde(default)]
    seconds: u64,
    /// Free text naming the machine that ran it; only logged.
    #[serde(default)]
    host: Option<String>,
    /// Failing test names; null when the build failed.
    #[serde(default)]
    names: Option<Vec<String>>,
    /// The build itself broke.
    #[serde(default)]
    build_failed: bool,
    /// Output tail kept for a failed build.
    #[serde(default)]
    tail: String,
}

impl ExternalResult {
    /// The result as the reply text [`finish_gate`] reads.
    fn reply(&self) -> String {
        if self.build_failed {
            return format!("{BEGIN}\n{BUILD_FAILED}\n{END}\n{}", self.tail);
        }
        match &self.names {
            Some(names) => format!("{BEGIN}\n{}\n{END}", names.join("\n")),
            None => String::new(),
        }
    }
}

/// Parsed `*.json` results in `dir`, oldest modification time first. A file
/// that cannot be read or parsed is logged once and moved to `<dir>/bad/`, so
/// an operator sees it and it is not re-read every tick.
fn pending_results(ctx: &Ctx, dir: &Path) -> Vec<(PathBuf, ExternalResult)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(u64, PathBuf, ExternalResult)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json") && path.is_file())
        .filter_map(|path| {
            let parsed = fs::read_to_string(&path)
                .map_err(|err| err.to_string())
                .and_then(|text| serde_json::from_str(&text).map_err(|err| err.to_string()));
            match parsed {
                Ok(result) => Some((file_mtime(&path), path, result)),
                Err(err) => {
                    set_aside(ctx, dir, &path, &err);
                    None
                }
            }
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    found
        .into_iter()
        .map(|(_, path, result)| (path, result))
        .collect()
}

/// Moves an unusable external result to `<dir>/bad/` and logs why.
fn set_aside(ctx: &Ctx, dir: &Path, path: &Path, why: &str) {
    let bad = dir.join("bad");
    let moved = fs::create_dir_all(&bad)
        .and_then(|()| fs::rename(path, bad.join(path.file_name().unwrap_or_default())));
    match moved {
        Ok(()) => ctx.log(&format!(
            "external gate result {} unreadable ({why}); moved to bad/",
            path.display()
        )),
        Err(err) => ctx.log(&format!(
            "external gate result {} unreadable ({why}); move to bad/ failed: {err}",
            path.display()
        )),
    }
}

/// `true` while a due gate should keep waiting for an external result:
/// `results_dir` is set and the wait, started the first time a gate was due,
/// is younger than `results_wait_seconds`. Starts the wait when it is not
/// running yet; an elapsed wait is cleared so the pool gate goes out.
fn waiting_for_external(ctx: &Ctx, st: &mut State) -> bool {
    if ctx.cfg.gate.results_dir.is_none() {
        return false;
    }
    let now = ctx.now();
    let since = st
        .extra
        .get(WAIT_KEY)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(now);
    if now.saturating_sub(since) < ctx.cfg.gate.results_wait_seconds {
        st.extra
            .insert(String::from(WAIT_KEY), serde_json::json!(since));
        return true;
    }
    st.extra.remove(WAIT_KEY);
    false
}

/// Start a gate when one is due: send the gate brief as a task
/// and dispatch it read-only on the first gate pool with a free slot. Does
/// nothing while a gate task is open, when no pool is free, when the remote
/// tip is not a 40-hex commit, or while a due gate is still inside its
/// `results_wait_seconds` wait for an external result (a pool gate is only
/// the fallback). A failed dispatch closes the task and
/// records nothing; a successful one clears `FORCE-GATE` (when it forced the
/// gate) and stores the run in `last_gate`.
pub fn start_gate(ctx: &Ctx, st: &mut State) {
    if st.has_active_gate() {
        return;
    }
    force_after_fix(ctx, st);
    let force = ctx.flag(&force_flag(ctx));
    if !gate_due(ctx, st, force) || waiting_for_external(ctx, st) {
        return;
    }
    let Some(pool) = crate::routing::RoleRouting::first_free(
        ctx.routing.gate().and_then(|role| role.pool.as_deref()),
        &ctx.cfg.gate.pools,
        &slots(ctx),
    ) else {
        return;
    };
    let Some(tip) = remote_tip(ctx) else {
        return;
    };
    let Some(path) = write_gate_brief(ctx) else {
        return;
    };
    let Some(task) = send_gate_task(ctx, &path, &tip) else {
        return;
    };
    if !dispatch_gate(ctx, &pool, &task, &tip) {
        return;
    }
    if force {
        remove_flag(ctx, &force_flag(ctx));
    }
    st.last_gate = GateRun {
        at: ctx.now(),
        merges: st.merges,
        task: Some(task.clone()),
        tip: Some(tip.clone()),
        pool: Some(pool.clone()),
    };
    let short = tip_short(&tip);
    ctx.log(&format!("gate started at {short} on {pool} task={task}"));
}

/// Force the next gate when a fix unit of this target merged after the last
/// gate started: a merged fix is the path back to green and should not wait
/// for the merge-count or clock rules. Writes `FORCE-GATE` once per merged
/// fix (starting the gate moves `last_gate.at` past the merge); does nothing
/// while a gate runs or the flag is already set.
fn force_after_fix(ctx: &Ctx, st: &State) {
    if st.has_active_gate() || ctx.flag(&force_flag(ctx)) {
        return;
    }
    let prefix = ctx.cfg.fix_prefix.as_str();
    let merged_at = |unit: &Unit| unit.merged_at.unwrap_or(unit.since);
    let newest = st
        .units
        .iter()
        .filter(|(uid, unit)| {
            uid.starts_with(prefix)
                && unit.status == Status::Merged
                && unit.target_id() == ctx.target_id
                && merged_at(unit) > st.last_gate.at
        })
        .max_by_key(|(_, unit)| merged_at(unit));
    if let Some((uid, _)) = newest {
        write_force_flag(ctx);
        ctx.log(&format!("gate forced: fix {uid} merged"));
    }
}

/// The due rules: a `FORCE-GATE` flag, no baseline yet, enough merges since
/// the last gate, or new merges with the last gate old enough.
fn gate_due(ctx: &Ctx, st: &State, force: bool) -> bool {
    let count_due =
        st.merges.saturating_sub(st.last_gate.merges) >= u64::from(ctx.cfg.gate.every_merges);
    let clock_due = st.merges > st.last_gate.merges
        && ctx.now().saturating_sub(st.last_gate.at) > ctx.cfg.gate.every_seconds;
    force || st.baseline.is_none() || count_due || clock_due
}

/// The remote integration tip: first token of
/// `git ls-remote <remote> refs/heads/<branch>`, when it is 40 hex characters.
fn remote_tip(ctx: &Ctx) -> Option<String> {
    let out = ctx.git(
        &format!("ls-remote {} refs/heads/{}", ctx.cfg.remote, ctx.cfg.branch),
        60,
    );
    let tip = out.text.split_whitespace().next().unwrap_or_default();
    if tip.len() == 40 && tip.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(tip.to_owned())
    } else {
        None
    }
}

/// Write the rendered gate brief to `<home>/briefs/gate.txt`.
fn write_gate_brief(ctx: &Ctx) -> Option<PathBuf> {
    let path = ctx.home.join("briefs").join("gate.txt");
    if let Some(dir) = path.parent()
        && let Err(err) = fs::create_dir_all(dir)
    {
        ctx.log(&format!("gate brief dir failed: {err}"));
        return None;
    }
    if let Err(err) = fs::write(&path, gate_brief(ctx)) {
        ctx.log(&format!("gate brief write failed: {err}"));
        return None;
    }
    Some(path)
}

/// Send the gate task; the subject names the command and the tip. Returns the
/// created task handle.
fn send_gate_task(ctx: &Ctx, path: &Path, tip: &str) -> Option<String> {
    let to = ctx
        .routing
        .gate()
        .map_or_else(|| ctx.cfg.to.clone(), |role| role.name.clone());
    let subject = format!("[pump gate] {} at {}", ctx.cfg.gate.command, tip_short(tip));
    let out = ctx.vivi(
        &format!(
            "task send --from {} --to {} --subject {} --body-file {}",
            shell_quote(&ctx.cfg.from),
            shell_quote(&to),
            shell_quote(&subject),
            shell_quote(&path.to_string_lossy()),
        ),
        120,
    );
    parse_created_handle(&out.text)
}

/// Dispatch the gate task read-only from `tip`. On failure, log the reply
/// tail and close the task; `false` means the gate did not start.
fn dispatch_gate(ctx: &Ctx, pool: &str, task: &str, tip: &str) -> bool {
    let out = ctx.vivi(
        &format!(
            "pool dispatch --pool {pool} {task} --model {} --read-only --from {tip} \
             --budget {} --porcelain",
            ctx.cfg.gate.model, ctx.cfg.gate.budget,
        ),
        180,
    );
    if out.code == 0 && out.text.contains("dispatched") {
        return true;
    }
    let count = out.text.chars().count();
    let tail: String = out.text.chars().skip(count.saturating_sub(200)).collect();
    ctx.log(&format!(
        "gate dispatch failed: {}",
        tail.replace('\n', " | ")
    ));
    drop(ctx.vivi(
        &format!(
            "task done {task} --for {} --note 'dispatch failed'",
            ctx.cfg.from
        ),
        120,
    ));
    false
}

/// The forced-gate flag file name for this context's target: `FORCE-GATE`
/// for `default`, `FORCE-GATE-<target>` otherwise.
fn force_flag(ctx: &Ctx) -> String {
    if ctx.target_id == "default" {
        FORCE_FLAG.to_owned()
    } else {
        format!("{FORCE_FLAG}-{}", ctx.target_id)
    }
}

/// The first ten characters of a tip, for log and subject lines.
fn tip_short(tip: &str) -> String {
    tip.chars().take(10).collect()
}

/// Finish the open gate from its receipt: classify the reply,
/// keep the owned-red map honest, then set the baseline, report new failures
/// with a gate-fix planner, or promote a clean gate. The gate task is cleared
/// whatever the reply said.
pub fn finish_gate(ctx: &Ctx, st: &mut State, text: &str) {
    let names = parse_failing(text);
    log_markerless(ctx, text, names.as_deref());
    record_red_after_fix(ctx, st, names.as_deref());
    let run = st.last_gate.clone();
    let tip = run.tip.clone().unwrap_or_default();
    let mut rec = GateRecord {
        tip: tip.clone(),
        at: ctx.now(),
        merges: run.merges,
        names: names.clone(),
        new: None,
    };
    st.last_gate.task = None;
    if let Some(kind) = unreadable_kind(names.as_deref()) {
        ctx.exception(kind, &tip_short(&tip), &tail_chars(text, 500));
        st.gates.push(rec);
        return;
    }
    let ignored = ignored_names(ctx);
    let names: Vec<String> = names
        .unwrap_or_default()
        .into_iter()
        .filter(|name| !ignored.contains(name))
        .collect();
    rec.names = Some(names.clone());
    if st.baseline.is_none() {
        st.baseline = Some(names.clone());
        ctx.log(&format!(
            "gate baseline at {}: {} failing names",
            tip_short(&tip),
            names.len()
        ));
        st.gates.push(rec);
        return;
    }
    finish_known_gate(ctx, st, &mut rec, &names);
    st.gates.push(rec);
}

/// `Some(kind)` when the reply cannot be trusted: no names at all is
/// `gate-unreadable`, a lone `BUILD-FAILED` line is `gate-build-failed`.
fn unreadable_kind(names: Option<&[String]>) -> Option<&'static str> {
    match names {
        None => Some("gate-unreadable"),
        Some([one]) if one.as_str() == BUILD_FAILED => Some("gate-build-failed"),
        Some(_) => None,
    }
}

/// The log line python emits when the names came from the reply text rather
/// than the markers.
fn log_markerless(ctx: &Ctx, text: &str, names: Option<&[String]>) {
    if let Some(names) = names
        && last_usable_block(text).is_none()
        && !names.is_empty()
    {
        ctx.log(&format!(
            "gate markers missing; read {} names from the reply text",
            names.len()
        ));
    }
}

/// The `red-after-fix` screen: an owned name that still
/// fails while every unit of its owner need is settled is reported once.
fn record_red_after_fix(ctx: &Ctx, st: &mut State, names: Option<&[String]>) {
    let Some(names) = names else {
        return;
    };
    let owned: Vec<(String, String)> = st
        .red
        .iter()
        .map(|(name, owner)| (name.clone(), owner.clone()))
        .collect();
    for (name, owner) in owned {
        let statuses: Vec<Status> = st
            .units
            .values()
            .filter(|unit| unit.need == owner)
            .map(|unit| unit.status)
            .collect();
        let mark = format!("{owner}:{name}");
        let failing = names.iter().any(|n| n == &name);
        if statuses.is_empty()
            || !failing
            || !statuses.iter().all(|&s| is_settled(s))
            || st.red_reported.contains(&mark)
            || replans_left(ctx, st, &name)
        {
            continue;
        }
        st.red_reported.push(mark);
        ctx.exception("red-after-fix", &owner, &name);
    }
}

/// A readable gate against an existing baseline: release
/// owned names that pass again, compute the new failures and the residual
/// ones (owned names whose owner settled but that still fail), then either
/// plan them (owning each name and starting a gate-fix planner) or promote.
fn finish_known_gate(ctx: &Ctx, st: &mut State, rec: &mut GateRecord, names: &[String]) {
    drop_fixed_names(ctx, st, names);
    let new = new_failures(st, names);
    rec.new = Some(new.clone());
    let residual = residual_failures(ctx, st, names);
    if !new.is_empty() || !residual.is_empty() {
        let owner = send_fix_need(ctx, &rec.tip, &new, &residual)
            .unwrap_or_else(|| plan_need(ctx, &rec.tip, !residual.is_empty()));
        for name in &new {
            st.red.insert(name.clone(), owner.clone());
        }
        let clean = last_clean_tip(st, &rec.tip);
        take_residuals(ctx, st, &owner, &residual);
        gatefix(ctx, &rec.tip, &clean, &owner, &new, &residual);
    }
    if new.is_empty() {
        ctx.log(&gate_summary(rec, names, 0));
        note_unknown_main_set(ctx, st);
        let off_main = not_on_main(st, names);
        if off_main.is_empty() {
            promote(ctx, st, &rec.tip, names);
        } else {
            green::promote_last_green(ctx, st, &rec.tip, names);
            ctx.log(&held_message(&off_main, &rec.tip));
        }
        return;
    }
    report_new_failures(
        ctx,
        rec,
        &new,
        &suspect_ids(st, st.gates.last().map_or(0, |g| g.merges), rec.merges),
    );
    green::promote_last_green(ctx, st, &rec.tip, names);
    ctx.log(&gate_summary(rec, names, new.len()));
}

/// Log once per tip when the newest promotion has no recorded failing set
/// (a legacy promotion), so main's set falls back to the baseline visibly.
fn note_unknown_main_set(ctx: &Ctx, st: &mut State) {
    let Some(promotion) = st.promotions.last() else {
        return;
    };
    let tip = promotion.tip.clone();
    let logged = st.extra.get(UNKNOWN_KEY).and_then(|v| v.as_str());
    if green::promoted_set(&st.promotions, &st.gates).is_some() || logged == Some(tip.as_str()) {
        return;
    }
    ctx.log(&format!(
        "promote: main's failing set unknown at {}; using baseline",
        tip_short(&tip)
    ));
    st.extra
        .insert(UNKNOWN_KEY.to_owned(), serde_json::Value::String(tip));
}

/// The gate's failing names that main does not carry, sorted. Main is
/// promoted only when there are none: main never gets redder.
fn not_on_main(st: &State, names: &[String]) -> Vec<String> {
    let main = green::main_set(&st.promotions, &st.gates, st.baseline.as_deref());
    names
        .iter()
        .filter(|name| !main.contains(*name))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The `promote held` log line, naming up to five of the offending names.
fn held_message(off_main: &[String], tip: &str) -> String {
    let shown = off_main.iter().take(5).cloned().collect::<Vec<_>>();
    let more = off_main.len().saturating_sub(shown.len());
    let tail = if more > 0 {
        format!(" and {more} more")
    } else {
        String::new()
    };
    format!(
        "promote held: {} failures not on main at {} (main never gets redder): {}{tail}",
        off_main.len(),
        tip_short(tip),
        shown.join("; ")
    )
}

/// The `gate at <tip10>: <n> failing, <m> new` log line.
fn gate_summary(rec: &GateRecord, names: &[String], new: usize) -> String {
    format!(
        "gate at {}: {} failing, {new} new",
        tip_short(&rec.tip),
        names.len()
    )
}

/// Owned names that no longer fail are released.
fn drop_fixed_names(ctx: &Ctx, st: &mut State, names: &[String]) {
    let fixed: Vec<String> = st
        .red
        .keys()
        .filter(|name| !names.iter().any(|n| n == *name))
        .cloned()
        .collect();
    for name in fixed {
        if let Some(owner) = st.red.remove(&name) {
            ctx.log(&format!("red fixed: {name} (owner {owner})"));
        }
    }
}

/// The names in neither the baseline nor the owned map,
/// sorted.
fn new_failures(st: &State, names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|name| {
            !st.baseline.as_ref().is_some_and(|b| b.contains(*name)) && !st.red.contains_key(*name)
        })
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The names in `[pump.gate] ignore_file`: one per line, trimmed, blank lines
/// and `#` comments skipped. Empty when unset or unreadable.
fn ignored_names(ctx: &Ctx) -> BTreeSet<String> {
    let Some(path) = ctx.cfg.ignore_file(&ctx.project) else {
        return BTreeSet::new();
    };
    match fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(String::from)
            .collect(),
        Err(_) => BTreeSet::new(),
    }
}

/// The merged units in `st.order[prev..merges]`: the merges
/// between the previous gate and this one.
fn suspect_ids(st: &State, prev: u64, merges: u64) -> Vec<String> {
    let list: Vec<&String> = st
        .order
        .iter()
        .filter(|uid| {
            st.units
                .get(*uid)
                .is_some_and(|u| u.status == Status::Merged)
        })
        .collect();
    let start = usize::try_from(prev).unwrap_or(usize::MAX).min(list.len());
    let end = usize::try_from(merges)
        .unwrap_or(usize::MAX)
        .min(list.len());
    if start >= end {
        Vec::new()
    } else {
        list[start..end]
            .iter()
            .map(|s| String::from(s.as_str()))
            .collect()
    }
}

/// Own every new name, then raise the `gate-new-failures` exception with the
/// suspect merge window.
fn report_new_failures(ctx: &Ctx, rec: &GateRecord, new: &[String], suspects: &[String]) {
    let joined = cut_chars(&new.join("; "), 350);
    let ids = cut_chars(&suspects.join(" "), 200);
    ctx.exception(
        "gate-new-failures",
        &tip_short(&rec.tip),
        &format!("{} new: {joined} ; suspects: {ids}", new.len()),
    );
}

/// The tip of the last gate with readable names and no new failure, else the
/// first gate's tip, else the current one.
fn last_clean_tip(st: &State, current: &str) -> String {
    for gate in st.gates.iter().rev() {
        if gate.names.is_some() && gate.new.as_ref().is_none_or(Vec::is_empty) {
            return gate.tip.clone();
        }
    }
    st.gates
        .first()
        .map_or_else(|| current.to_owned(), |gate| gate.tip.clone())
}

/// Fast-forward main to a gated tip of the integration branch.
/// Never forces: a main that moved elsewhere waits for the next batch.
fn promote(ctx: &Ctx, st: &mut State, tip: &str, failing: &[String]) {
    if !ctx.cfg.promote || ctx.cfg.main.is_empty() || !main_is_behind(ctx, tip) {
        return;
    }
    let out = ctx.git(
        &format!("push {} {tip}:refs/heads/{}", ctx.cfg.remote, ctx.cfg.main),
        180,
    );
    let tail = cut_chars(last_line(&out.text).trim(), 120);
    ctx.log(&format!(
        "promote {} -> {} rc={} {tail}",
        ctx.cfg.main,
        tip_short(tip),
        out.code
    ));
    if out.code == 0 {
        st.promotions.push(Promotion {
            tip: tip.to_owned(),
            at: ctx.now(),
            failing: Some(failing.to_vec()),
        });
    }
}

/// `true` when remote main is an ancestor of the gated `tip`, so the push is a
/// fast-forward. Fetches main and the integration branch first: another merge
/// lane's clone may have pushed the tip, which this checkout lacks until it
/// fetches. `merge-base --is-ancestor` exits 0 for an ancestor and 1 when main
/// moved; a failed fetch or any other exit is a logged `promote error`.
fn main_is_behind(ctx: &Ctx, tip: &str) -> bool {
    let (remote, main) = (&ctx.cfg.remote, &ctx.cfg.main);
    let fetch = ctx.git(&format!("fetch -q {remote} {main} {}", ctx.cfg.branch), 120);
    if fetch.code != 0 {
        ctx.log(&format!(
            "promote error: {}",
            tail_chars(fetch.text.trim(), 120)
        ));
        return false;
    }
    let check = ctx.git(
        &format!("merge-base --is-ancestor {remote}/{main} {tip}"),
        120,
    );
    match check.code {
        0 => return true,
        1 => ctx.log(&format!(
            "promote skipped: main has commits {} does not have yet",
            ctx.cfg.branch
        )),
        _ => ctx.log(&format!(
            "promote error: {}",
            tail_chars(check.text.trim(), 120)
        )),
    }
    false
}

/// The last line of a command's output, python's
/// `out.strip().splitlines()[-1]`.
fn last_line(text: &str) -> String {
    text.trim()
        .lines()
        .next_back()
        .map_or_else(String::new, String::from)
}

/// While merging is held: once every fix unit has settled, a
/// clean gate newer than the `HOLD-MERGE` flag lifts the hold; fixes merged
/// after the last gate (or an unreadable one) force a new gate.
pub fn hold_cycle(ctx: &Ctx, st: &mut State) {
    if !ctx.flag(HOLD_FLAG) || st.last_gate.task.is_some() || ctx.flag(&force_flag(ctx)) {
        return;
    }
    let prefix = ctx.cfg.fix_prefix.as_str();
    if !all_fixes_settled(st, prefix) {
        return;
    }
    let Some(last) = st.gates.last().cloned() else {
        return;
    };
    if clean_gate(&last) && last.at > file_mtime(&ctx.home.join(HOLD_FLAG)) {
        remove_flag(ctx, HOLD_FLAG);
        ctx.log(&format!(
            "hold lifted: gate at {} is clean",
            tip_short(&last.tip)
        ));
        return;
    }
    if newest_merged_fix(st, prefix) > last.at || last.names.is_none() {
        write_force_flag(ctx);
        ctx.log("hold: fixes merged, forcing a gate");
    }
}

/// Every `<fix_prefix>`-prefixed unit has a settled status.
fn all_fixes_settled(st: &State, prefix: &str) -> bool {
    st.units
        .iter()
        .filter(|(uid, _)| uid.starts_with(prefix))
        .all(|(_, unit)| is_settled(unit.status))
}

/// The newest `since` among merged fix units, or 0.
fn newest_merged_fix(st: &State, prefix: &str) -> u64 {
    st.units
        .iter()
        .filter(|(uid, unit)| uid.starts_with(prefix) && unit.status == Status::Merged)
        .map(|(_, unit)| unit.since)
        .max()
        .unwrap_or(0)
}

/// A gate record with readable names and no new failure.
fn clean_gate(gate: &GateRecord) -> bool {
    gate.names.is_some() && gate.new.as_ref().is_none_or(Vec::is_empty)
}

/// `true` for the statuses a fix unit ends in without further work: merged,
/// stuck, no-defect or replaced.
pub(crate) fn is_settled(status: Status) -> bool {
    matches!(
        status,
        Status::Merged | Status::Stuck | Status::NoDefect | Status::Replaced
    )
}

/// The flag file's modification time as epoch seconds, 0 when unknown.
fn file_mtime(path: &Path) -> u64 {
    match fs::metadata(path) {
        Ok(meta) => match meta.modified() {
            Ok(when) => when
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |span| span.as_secs()),
            Err(_) => 0,
        },
        Err(_) => 0,
    }
}

/// Write the `FORCE-GATE` flag with its payload, logging a failure.
fn write_force_flag(ctx: &Ctx) {
    if let Err(err) = fs::write(ctx.home.join(force_flag(ctx)), FORCE_TEXT) {
        ctx.log(&format!("gate flag write failed: {err}"));
    }
}

/// Remove a flag file, logging a failure.
fn remove_flag(ctx: &Ctx, name: &str) {
    if let Err(err) = fs::remove_file(ctx.home.join(name)) {
        ctx.log(&format!("gate flag remove failed: {err}"));
    }
}

/// The last `limit` characters of `text`.
fn tail_chars(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(limit)).collect()
}

/// The first `limit` characters of `text`.
fn cut_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}
