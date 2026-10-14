//! Gate start, finish, gate-fix, the hold cycle and promotion.
//!
//! Ports pump.py's gate: `start_gate` (872-904), `finish_gate` (962-1009)
//! with `gatefix` (919) and `promote` (949), and the hold cycle (931).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::pump::brief::gate_brief;
use crate::pump::collect::spawn_planner;
use crate::pump::dispatch::{parse_created_handle, slots};
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::state::{GateRecord, GateRun, Promotion, State, Status};

/// The `FAILING-BEGIN` reply marker.
const BEGIN: &str = "FAILING-BEGIN";
/// The `FAILING-END` reply marker.
const END: &str = "FAILING-END";
/// The single line a gate worker replies with when the build itself broke.
const BUILD_FAILED: &str = "BUILD-FAILED";
/// Flag file that makes the next tick start a gate whatever the due rules.
const FORCE_FLAG: &str = "FORCE-GATE";
/// Flag file that holds the merge queue until a clean gate.
const HOLD_FLAG: &str = "HOLD-MERGE";
/// The payload the hold cycle writes into `FORCE-GATE`.
const FORCE_TEXT: &str = "after fixes\n";

/// Read the failing-test names from a gate reply (pump.py:962).
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
    if pairs.is_empty() {
        None
    } else {
        Some(pairs)
    }
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

/// The fallback of pump.py:969: every `<crate> <path>` pair where the crate
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
        found.insert(format!("{} {}", &text[i..crate_end], &text[crate_end + 1..path_end]));
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
    if leading >= 2 {
        after_last
    } else {
        None
    }
}

/// Start a gate when one is due (pump.py:872): send the gate brief as a task
/// and dispatch it read-only on the first gate pool with a free slot. Does
/// nothing while a gate task is open, when no pool is free, or when the
/// remote tip is not a 40-hex commit. A failed dispatch closes the task and
/// records nothing; a successful one clears `FORCE-GATE` (when it forced the
/// gate) and stores the run in `last_gate`.
pub fn start_gate(ctx: &Ctx, st: &mut State) {
    if st.last_gate.task.is_some() {
        return;
    }
    let force = ctx.flag(FORCE_FLAG);
    if !gate_due(ctx, st, force) {
        return;
    }
    let Some(pool) = pick_pool(ctx) else {
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
        remove_force_flag(ctx);
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

/// The due rules: a `FORCE-GATE` flag, no baseline yet, enough merges since
/// the last gate, or new merges with the last gate old enough.
fn gate_due(ctx: &Ctx, st: &State, force: bool) -> bool {
    let count_due = st.merges.saturating_sub(st.last_gate.merges)
        >= u64::from(ctx.cfg.gate.every_merges);
    let clock_due = st.merges > st.last_gate.merges
        && ctx.now().saturating_sub(st.last_gate.at) > ctx.cfg.gate.every_seconds;
    force || st.baseline.is_none() || count_due || clock_due
}

/// The first configured gate pool with a free slot.
fn pick_pool(ctx: &Ctx) -> Option<String> {
    let free = slots(ctx);
    ctx.cfg
        .gate
        .pools
        .iter()
        .find(|pool| free.get(*pool).is_some_and(|&n| n > 0))
        .cloned()
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
    let short = tip_short(tip);
    let subject = format!("[pump gate] {} at {short}", ctx.cfg.gate.command);
    let out = ctx.vivi(
        &format!(
            "task send --from {} --to {} --subject {} --body-file {}",
            ctx.cfg.from,
            ctx.cfg.to,
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
    let tail: String = out
        .text
        .chars()
        .skip(count.saturating_sub(200))
        .collect();
    ctx.log(&format!(
        "gate dispatch failed: {}",
        tail.replace('\n', " | ")
    ));
    ctx.vivi(
        &format!("task done {task} --for {} --note 'dispatch failed'", ctx.cfg.from),
        120,
    );
    false
}

/// Remove the `FORCE-GATE` flag, logging when that fails.
fn remove_force_flag(ctx: &Ctx) {
    if let Err(err) = fs::remove_file(ctx.home.join(FORCE_FLAG)) {
        ctx.log(&format!("gate flag remove failed: {err}"));
    }
}

/// The first ten characters of a tip, for log and subject lines.
fn tip_short(tip: &str) -> String {
    tip.chars().take(10).collect()
}

/// Finish the open gate from its receipt (pump.py:962): classify the reply,
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
    let names = names.unwrap_or_default();
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
/// than the markers (pump.py:970).
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

/// The `red-after-fix` screen (pump.py:976-980): an owned name that still
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
        let prefix = format!("{owner}-");
        let statuses: Vec<Status> = st
            .units
            .iter()
            .filter(|(uid, _)| uid.starts_with(&prefix))
            .map(|(_, unit)| unit.status)
            .collect();
        let mark = format!("{owner}:{name}");
        let failing = names.iter().any(|n| n == &name);
        if statuses.is_empty()
            || !failing
            || !statuses.iter().all(|&s| is_settled(s))
            || st.red_reported.contains(&mark)
        {
            continue;
        }
        st.red_reported.push(mark);
        ctx.exception("red-after-fix", &owner, &name);
    }
}

/// A readable gate against an existing baseline (pump.py:988-1006): release
/// owned names that pass again, compute the new failures, then either report
/// them (owning each and starting a gate-fix planner) or promote.
fn finish_known_gate(ctx: &Ctx, st: &mut State, rec: &mut GateRecord, names: &[String]) {
    drop_fixed_names(ctx, st, names);
    let new = new_failures(ctx, st, names);
    rec.new = Some(new.clone());
    if new.is_empty() {
        ctx.log(&gate_summary(rec, names, 0));
        promote(ctx, st, &rec.tip);
        return;
    }
    let owner = fix_need(ctx, &rec.tip);
    for name in &new {
        st.red.insert(name.clone(), owner.clone());
    }
    let clean = last_clean_tip(st, &rec.tip);
    gatefix(ctx, st, &rec.tip, &clean, &new);
    report_new_failures(ctx, rec, &new, &suspect_ids(st, st.gates.last().map_or(0, |g| g.merges), rec.merges));
    ctx.log(&gate_summary(rec, names, new.len()));
}

/// The `gate at <tip10>: <n> failing, <m> new` log line.
fn gate_summary(rec: &GateRecord, names: &[String], new: usize) -> String {
    format!(
        "gate at {}: {} failing, {new} new",
        tip_short(&rec.tip),
        names.len()
    )
}

/// Owned names that no longer fail are released (pump.py:991-993).
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

/// The names in neither the baseline, the known-red list nor the owned map,
/// sorted (pump.py:994).
fn new_failures(ctx: &Ctx, st: &State, names: &[String]) -> Vec<String> {
    let known = known_red(ctx);
    names
        .iter()
        .filter(|name| {
            !known.contains(*name)
                && !st.baseline.as_ref().is_some_and(|b| b.contains(*name))
                && !st.red.contains_key(*name)
        })
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The lines of `<home>/known-red.txt`, when it exists.
fn known_red(ctx: &Ctx) -> BTreeSet<String> {
    match fs::read_to_string(ctx.home.join("known-red.txt")) {
        Ok(text) => text.lines().map(String::from).collect(),
        Err(_) => BTreeSet::new(),
    }
}

/// The merged units in `st.order[prev..merges]` (pump.py:996): the merges
/// between the previous gate and this one.
fn suspect_ids(st: &State, prev: u64, merges: u64) -> Vec<String> {
    let list: Vec<&String> = st
        .order
        .iter()
        .filter(|uid| st.units.get(*uid).is_some_and(|u| u.status == Status::Merged))
        .collect();
    let start = usize::try_from(prev).unwrap_or(usize::MAX).min(list.len());
    let end = usize::try_from(merges).unwrap_or(usize::MAX).min(list.len());
    if start >= end {
        Vec::new()
    } else {
        list[start..end].iter().map(|s| String::from(s.as_str())).collect()
    }
}

/// Own every new name, then raise the `gate-new-failures` exception with the
/// suspect merge window (pump.py:1001-1005).
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
/// first gate's tip, else the current one (pump.py:1000).
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

/// The gate-fix need id for a tip: `<fix_prefix>-g<tip8>`.
fn fix_need(ctx: &Ctx, tip: &str) -> String {
    let short: String = tip.chars().take(8).collect();
    format!("{}-g{short}", ctx.cfg.fix_prefix)
}

/// Start the gate-fix planner for a red gate (pump.py:919): need id
/// `<fix_prefix>-g<tip8>`, skipped when its plan file already exists.
fn gatefix(ctx: &Ctx, _st: &State, tip: &str, clean: &str, new: &[String]) {
    let need = fix_need(ctx, tip);
    if ctx.home.join("plans").join(format!("{need}.json")).exists() {
        return;
    }
    let prompt = gatefix_prompt(ctx, tip, clean, &need, new);
    spawn_planner(ctx, "gatefix", &need, &prompt);
    ctx.log(&format!("gatefix planner started for {need} ({} new)", new.len()));
}

/// The gate-fix planner prompt (pump.py:907), with the repo, home and plan
/// paths taken from the context.
fn gatefix_prompt(ctx: &Ctx, tip: &str, clean: &str, need: &str, new: &[String]) -> String {
    let names = new
        .iter()
        .map(|name| format!("- {name}"))
        .collect::<Vec<_>>()
        .join("\n");
    let repo = ctx.repo.display();
    let home = ctx.home.display();
    let remote = &ctx.cfg.remote;
    format!(
        "The integration branch went red. Plan the fix units, as your system prompt describes, with these rules.

A full test run at integration-branch commit {tip} shows these failing tests that were NOT failing at the last clean run (commit {clean}):
{names}

The commits merged between the two are `git log --oneline {clean}..{tip}` in {repo} (run `git -C {repo} fetch -q {remote}` first; each merge subject names its unit and need). Read the failing tests and the merged diffs and find, for each failure, the merge that most likely caused it.

Write {home}/plans/{need}.json (write .tmp, then mv): {{\"need\": \"{need}\", \"verdict\": \"units\", \"reason\": \"...\", \"units\": [...]}}. Unit ids \"{need}-1\", \"{need}-2\", ... . Group failures with one likely cause into one unit; one cause per unit. Each brief names the failing tests, the suspected merge commit and why, and says: run the tests first; fix the CAUSE forward; change a test only if the suspected unit implemented a ruling that deliberately changed the pinned behaviour, and name the ruling; never delete, ignore or weaken a test; name the culprit commit in the reply. Proof: the failing tests by name (one `cargo test` filter per command). Keep write scopes as narrow as you can.
Your final message is one line.
"
    )
}

/// Fast-forward main to a gated tip of the integration branch (pump.py:949).
/// Never forces: a main that moved elsewhere waits for the next batch.
fn promote(ctx: &Ctx, st: &mut State, tip: &str) {
    if !ctx.cfg.promote || ctx.cfg.main.is_empty() {
        return;
    }
    ctx.git(&format!("fetch -q {} {}", ctx.cfg.remote, ctx.cfg.main), 120);
    let head = format!("{}/{}", ctx.cfg.remote, ctx.cfg.main);
    if ctx.git(&format!("merge-base --is-ancestor {head} {tip}"), 120).code != 0 {
        ctx.log(&format!(
            "promote skipped: main has commits {} does not have yet",
            ctx.cfg.branch
        ));
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
        st.promotions
            .push(Promotion { tip: tip.to_owned(), at: ctx.now() });
    }
}

/// The last line of a command's output, python's
/// `out.strip().splitlines()[-1]`.
fn last_line(text: &str) -> String {
    text.trim()
        .lines()
        .next_back()
        .map_or_else(String::new, String::from)
}

/// While merging is held (pump.py:931): once every fix unit has settled, a
/// clean gate newer than the `HOLD-MERGE` flag lifts the hold; fixes merged
/// after the last gate (or an unreadable one) force a new gate.
pub fn hold_cycle(ctx: &Ctx, st: &mut State) {
    if !ctx.flag(HOLD_FLAG) || st.last_gate.task.is_some() || ctx.flag(FORCE_FLAG) {
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
        ctx.log(&format!("hold lifted: gate at {} is clean", tip_short(&last.tip)));
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
fn is_settled(status: Status) -> bool {
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
    if let Err(err) = fs::write(ctx.home.join(FORCE_FLAG), FORCE_TEXT) {
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
