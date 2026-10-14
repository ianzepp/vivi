//! Gate start, finish, gate-fix, the hold cycle and promotion.
//!
//! Ports pump.py's gate: `start_gate` (lines 872-904) and the reply parsing
//! at the top of `finish_gate` (lines 962-975). `finish_gate`, `hold_cycle`
//! and promotion land with later units.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::pump::brief::gate_brief;
use crate::pump::dispatch::{parse_created_handle, slots};
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::state::{GateRun, State};

/// The `FAILING-BEGIN` reply marker.
const BEGIN: &str = "FAILING-BEGIN";
/// The `FAILING-END` reply marker.
const END: &str = "FAILING-END";
/// The single line a gate worker replies with when the build itself broke.
const BUILD_FAILED: &str = "BUILD-FAILED";
/// Flag file that makes the next tick start a gate whatever the due rules.
const FORCE_FLAG: &str = "FORCE-GATE";

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
    if let Some(dir) = path.parent() {
        if let Err(err) = fs::create_dir_all(dir) {
            ctx.log(&format!("gate brief dir failed: {err}"));
            return None;
        }
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
