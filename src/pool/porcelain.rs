//! The `--porcelain` output of the pool commands: a stable contract for an
//! agent or a script.
//!
//! Standard output holds result lines and nothing else: no start lines, no
//! heartbeats, no headings. Each line is `<kind>` followed by fixed fields
//! separated by single spaces, in the order documented below. An absent field
//! is `-`. A value that is empty, contains whitespace, a quote or a backslash,
//! or is itself `-` is written in double quotes with `\"`, `\\`, `\n`, `\r`,
//! `\t` and `\u{hex}` escapes. Times are epoch seconds. Exit codes are those
//! of the default output.
//!
//! Every line that is about a pool carries POOL, the pool's name: it is the
//! first field after the line kind, except on the lines that lead with TASK
//! (`dispatched`, `collected`, `reconciled`), where it follows TASK.
//!
//! ```text
//! pool create    home POOL TEXT                                   (only when the home was made)
//!                created POOL NAME ID STATE SECONDS EXIT OUTCOME ERROR   OUTCOME: seeded|timeout|failed|unmarked
//!                exists POOL COUNT                                 (nothing to create)
//!                issue POOL KIND SUBJECT MESSAGE FIRST CLEARED
//! pool status    slot POOL NAME ID STATE TASK RUNNING BUDGET BASE ARCH CREATED MODE
//!                                                                  MODE: write|read-only, or - (no open dispatch)
//! pool scale     created POOL NAME ID STATE SECONDS EXIT OUTCOME ERROR   (as create; also home, exists, issue)
//!                destroyed POOL NAME ID
//!                skipped POOL NAME ID STATE TASK                   (a slot that was not free, so kept)
//!                scaled POOL TARGET COUNT                          (COUNT: slots the pool holds now)
//! pool dispatch  dispatched TASK POOL SLOT JOB MODEL FROM START BRANCH BUDGET MODE ABANDONED COST
//!                                                                  BRANCH: - when read-only; MODE: write|read-only;
//!                                                                  ABANDONED: pods made and terminated before the one that
//!                                                                  works because they did not answer on SSH in time (0 on
//!                                                                  every other host); COST: their approximate total charge
//!                                                                  in dollars, `0.12`, or - when none or no price known
//! pool wait      collected TASK POOL SLOT STATE DETAIL             STATE: finished|timeout|lost
//!                nothing-open
//!                issue POOL KIND SUBJECT MESSAGE FIRST CLEARED     KIND: poll-error
//! pool reconcile reconciled TASK POOL SLOT STATE DETAIL            STATE: running|finished|timeout|lost|error|stale-claim|orphan-stopped
//!                nothing-open
//! pool doctor    check POOL STATUS GROUP NAME DETAIL FIX MILLIS
//!                summary POOL OK WARN FAIL SKIPPED                 (one per pool)
//! pool clean     cleaned POOL SLOT KIND BYTES PATH NOTE            KIND: build|worktree|target|branch|orphan|lane-build|reset
//!                would POOL SLOT KIND BYTES PATH NOTE              (dry-run)
//!                skipped POOL SLOT KIND REASON
//!                disk FREE_BYTES
//!                summary ACTIONS SKIPS BYTES
//! pool reset     reset POOL SLOT BASE SHA WHY
//!                skipped POOL SLOT reset REASON
//! ```
//!
//! `DETAIL` of a finished job is its outcome: `pushed BRANCH SHA`,
//! `committed BRANCH SHA` (worktree pool), `no_changes`, `push_refused`, a
//! runner failure word, or, for a read-only job, `read_only_unchanged` (the
//! tree was as checked out) or `read_only_discarded COUNT` (changes were
//! discarded, COUNT paths; the paths are on the task's result record).
//!
//! A `POOL` of `-` appears only where the pool is not known: a dispatch
//! recorded without one (made before pools were named), or an issue that is
//! not about one pool.

use super::collect::RoundLine;
use super::doctor::{Check, Counts};
use super::progress::Issue;
use super::record::DispatchRecord;
use super::run::SlotOutcome;
use super::slot::SlotStatus;

/// One value of a line: bare when it is a plain word, else quoted.
#[must_use]
pub fn quote(value: &str) -> String {
    let plain = !value.is_empty()
        && value != "-"
        && !value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '\\');
    if plain {
        return value.to_string();
    }
    let mut out = String::from('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.extend(c.escape_unicode()),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A value that may be absent: `-` when it is.
fn opt(value: Option<&str>) -> String {
    value.map_or_else(|| "-".to_string(), quote)
}

fn num(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_string(), |n| n.to_string())
}

fn line(kind: &str, fields: &[String]) -> String {
    std::iter::once(kind.to_string())
        .chain(fields.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `dispatched TASK POOL SLOT JOB MODEL FROM START BRANCH BUDGET MODE ABANDONED COST`
#[must_use]
pub fn dispatched(task: &str, made: &DispatchRecord) -> String {
    let fields = [
        quote(task),
        quote(&made.pool),
        quote(&made.slot),
        quote(&made.job),
        quote(&made.model),
        quote(&made.from),
        quote(&made.start),
        opt(Some(made.branch.as_str()).filter(|b| !b.is_empty())),
        made.budget.to_string(),
        mode_word(made.read_only).to_string(),
        made.abandoned.to_string(),
        if made.abandoned_cost_micros == 0 {
            "-".to_string()
        } else {
            super::record::dollars(made.abandoned_cost_micros)
        },
    ];
    line("dispatched", &fields)
}

fn mode_word(read_only: bool) -> &'static str {
    if read_only { "read-only" } else { "write" }
}

/// `home POOL TEXT`
#[must_use]
pub fn home(pool: &str, text: &str) -> String {
    line("home", &[quote(pool), quote(text)])
}

/// `exists POOL COUNT`
#[must_use]
pub fn exists(pool: &str, count: usize) -> String {
    line("exists", &[quote(pool), count.to_string()])
}

/// `created POOL NAME ID STATE SECONDS EXIT OUTCOME ERROR`
#[must_use]
pub fn created(outcome: &SlotOutcome) -> String {
    let word = if outcome.seeded() {
        "seeded"
    } else if outcome.timed_out {
        "timeout"
    } else if outcome.error.is_some() {
        "unmarked"
    } else {
        "failed"
    };
    let exit = outcome.exit_code.map(|c| c.to_string());
    let fields = [
        quote(&outcome.pool),
        quote(&outcome.name),
        quote(&outcome.id),
        quote(&outcome.state),
        outcome.seconds.to_string(),
        exit.unwrap_or_else(|| "-".to_string()),
        word.to_string(),
        opt(outcome.error.as_deref()),
    ];
    line("created", &fields)
}

/// `destroyed POOL NAME ID`
#[must_use]
pub fn destroyed(slot: &SlotStatus) -> String {
    line(
        "destroyed",
        &[quote(&slot.pool), quote(&slot.name), quote(&slot.id)],
    )
}

/// `skipped POOL NAME ID STATE TASK`
#[must_use]
pub fn skipped(slot: &SlotStatus) -> String {
    let fields = [
        quote(&slot.pool),
        quote(&slot.name),
        quote(&slot.id),
        quote(&slot.state),
        opt(slot.task.as_deref()),
    ];
    line("skipped", &fields)
}

/// `scaled POOL TARGET COUNT`
#[must_use]
pub fn scaled(pool: &str, target: u32, count: usize) -> String {
    line(
        "scaled",
        &[quote(pool), target.to_string(), count.to_string()],
    )
}

/// `slot POOL NAME ID STATE TASK RUNNING BUDGET BASE ARCH CREATED MODE`
#[must_use]
pub fn slot(slot: &SlotStatus) -> String {
    let base = status_base(slot);
    let fields = [
        quote(&slot.pool),
        quote(&slot.name),
        quote(&slot.id),
        quote(&slot.state),
        opt(slot.task.as_deref()),
        num(slot.running_secs),
        num(slot.budget_secs),
        opt(Some(base.as_str()).filter(|s| !s.is_empty())),
        opt(slot.arch.as_deref()),
        quote(&slot.created_at),
        slot.read_only
            .map_or_else(|| "-".to_string(), |r| mode_word(r).to_string()),
    ];
    line("slot", &fields)
}

/// `capacity POOL FREE`: a pool whose slots exist only while a job runs
/// (worker, RunPod) lists no resting slots, so this says how many more jobs
/// it may start now: its `slots` less the pods it holds.
#[must_use]
pub fn capacity(pool: &str, free: u32) -> String {
    line("capacity", &[quote(pool), free.to_string()])
}

fn round(kind: &str, l: &RoundLine) -> String {
    let task = if l.task == "-" {
        "-".to_string()
    } else {
        quote(&l.task)
    };
    line(
        kind,
        &[
            task,
            opt(Some(l.pool.as_str()).filter(|p| !p.is_empty())),
            quote(&l.slot),
            quote(l.state),
            quote(&l.detail),
        ],
    )
}

/// `collected TASK POOL SLOT STATE DETAIL`
#[must_use]
pub fn collected(l: &RoundLine) -> String {
    round("collected", l)
}

/// `reconciled TASK POOL SLOT STATE DETAIL`
#[must_use]
pub fn reconciled(l: &RoundLine) -> String {
    round("reconciled", l)
}

/// `nothing-open`
#[must_use]
pub fn nothing_open() -> String {
    "nothing-open".to_string()
}

/// `issue POOL KIND SUBJECT MESSAGE FIRST CLEARED`
#[must_use]
pub fn issue(issue: &Issue) -> String {
    let fields = [
        opt(issue.pool.as_deref()),
        quote(issue.kind),
        opt(issue.subject.as_deref()),
        quote(&issue.message),
        issue.first_seen.to_string(),
        num(issue.cleared),
    ];
    line("issue", &fields)
}

/// `check POOL STATUS GROUP NAME DETAIL FIX MILLIS`
#[must_use]
pub fn check(check: &Check) -> String {
    let fields = [
        opt(Some(check.pool.as_str()).filter(|p| !p.is_empty())),
        check.status.word().to_string(),
        quote(&check.group),
        quote(&check.name),
        quote(&check.detail),
        opt(check.fix.as_deref()),
        num(check.millis),
    ];
    line("check", &fields)
}

/// `cleaned POOL SLOT KIND BYTES PATH NOTE` (or `would` when dry-run)
#[must_use]
pub fn cleaned(dry_run: bool, action: &super::clean::CleanAction) -> String {
    let kind = if dry_run { "would" } else { "cleaned" };
    line(
        kind,
        &[
            quote(&action.pool),
            quote(&action.slot),
            action.kind.word().to_string(),
            action.bytes.to_string(),
            quote(&action.path),
            quote(&action.note),
        ],
    )
}

/// `skipped POOL SLOT KIND REASON` for a clean that stayed put
#[must_use]
pub fn clean_skip(skip: &super::clean::CleanSkip) -> String {
    line(
        "skipped",
        &[
            quote(&skip.pool),
            quote(&skip.slot),
            skip.kind.word().to_string(),
            quote(&skip.reason),
        ],
    )
}

/// `disk FREE_BYTES`
#[must_use]
pub fn clean_disk(free_bytes: u64) -> String {
    line("disk", &[free_bytes.to_string()])
}

/// `reset POOL SLOT BASE SHA WHY`
#[must_use]
pub fn reset_line(pool: &str, slot: &str, out: &super::reset::ResetOutcome) -> String {
    line(
        "reset",
        &[
            quote(pool),
            quote(slot),
            quote(&out.base_ref),
            quote(&out.sha),
            quote(out.why),
        ],
    )
}

/// `skipped POOL SLOT reset REASON`
#[must_use]
pub fn reset_skip(pool: &str, slot: &str, base: &str, reason: &str) -> String {
    line(
        "skipped",
        &[
            quote(pool),
            quote(slot),
            "reset".to_string(),
            quote(&format!("{base}: {reason}")),
        ],
    )
}

fn status_base(slot: &super::slot::SlotStatus) -> String {
    match (slot.base_ref.as_deref(), slot.base.as_deref()) {
        (Some(name), Some(sha)) if !name.is_empty() && !sha.is_empty() => {
            format!("{name}@{sha}")
        }
        (_, Some(sha)) => sha.to_string(),
        (Some(name), None) if !name.is_empty() => name.to_string(),
        _ => String::new(),
    }
}

/// `summary ACTIONS SKIPS BYTES`
#[must_use]
pub fn clean_summary(actions: usize, skips: usize, bytes: u64) -> String {
    line(
        "summary",
        &[actions.to_string(), skips.to_string(), bytes.to_string()],
    )
}

/// `summary POOL OK WARN FAIL SKIPPED`
#[must_use]
pub fn summary(pool: &str, counts: &Counts) -> String {
    let fields = [
        quote(pool),
        counts.ok.to_string(),
        counts.warn.to_string(),
        counts.fail.to_string(),
        counts.skipped.to_string(),
    ];
    line("summary", &fields)
}

#[cfg(test)]
#[path = "porcelain_test.rs"]
mod tests;
