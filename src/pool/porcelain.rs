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
//! pool status    slot POOL NAME ID STATE TASK RUNNING BUDGET BASE ARCH CREATED
//! pool scale     created POOL NAME ID STATE SECONDS EXIT OUTCOME ERROR   (as create; also home, exists, issue)
//!                destroyed POOL NAME ID
//!                skipped POOL NAME ID STATE TASK                   (a slot that was not free, so kept)
//!                scaled POOL TARGET COUNT                          (COUNT: slots the pool holds now)
//! pool dispatch  dispatched TASK POOL SLOT JOB MODEL FROM START BRANCH BUDGET
//! pool wait      collected TASK POOL SLOT STATE DETAIL             STATE: finished|timeout|lost
//!                nothing-open
//!                issue POOL KIND SUBJECT MESSAGE FIRST CLEARED     KIND: poll-error
//! pool reconcile reconciled TASK POOL SLOT STATE DETAIL            STATE: running|finished|timeout|lost|error|stale-claim|orphan-stopped
//!                nothing-open
//! pool doctor    check POOL STATUS GROUP NAME DETAIL FIX MILLIS
//!                summary POOL OK WARN FAIL SKIPPED                 (one per pool)
//! ```
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

/// `dispatched TASK POOL SLOT JOB MODEL FROM START BRANCH BUDGET`
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
        quote(&made.branch),
        made.budget.to_string(),
    ];
    line("dispatched", &fields)
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

/// `slot POOL NAME ID STATE TASK RUNNING BUDGET BASE ARCH CREATED`
#[must_use]
pub fn slot(slot: &SlotStatus) -> String {
    let fields = [
        quote(&slot.pool),
        quote(&slot.name),
        quote(&slot.id),
        quote(&slot.state),
        opt(slot.task.as_deref()),
        num(slot.running_secs),
        num(slot.budget_secs),
        opt(slot.base.as_deref()),
        opt(slot.arch.as_deref()),
        quote(&slot.created_at),
    ];
    line("slot", &fields)
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
