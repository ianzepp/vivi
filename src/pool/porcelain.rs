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
//! ```text
//! pool create    home TEXT                                        (only when the home was made)
//!                created NAME ID STATE SECONDS EXIT OUTCOME ERROR   OUTCOME: seeded|timeout|failed|unmarked
//!                exists POOL COUNT                                 (nothing to create)
//!                issue KIND SUBJECT MESSAGE FIRST CLEARED
//! pool status    slot NAME ID STATE TASK RUNNING BUDGET BASE ARCH CREATED
//! pool dispatch  dispatched TASK SLOT JOB MODEL FROM START BRANCH BUDGET
//! pool wait      collected TASK SLOT STATE DETAIL                  STATE: finished|timeout|lost
//!                nothing-open
//!                issue KIND SUBJECT MESSAGE FIRST CLEARED          KIND: poll-error
//! pool reconcile reconciled TASK SLOT STATE DETAIL                 STATE: running|finished|timeout|lost|error|stale-claim|orphan-stopped
//!                nothing-open
//! pool doctor    check STATUS GROUP NAME DETAIL FIX MILLIS
//!                summary OK WARN FAIL SKIPPED
//! ```

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

/// `dispatched TASK SLOT JOB MODEL FROM START BRANCH BUDGET`
#[must_use]
pub fn dispatched(task: &str, made: &DispatchRecord) -> String {
    let fields = [
        quote(task),
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

/// `home TEXT`
#[must_use]
pub fn home(text: &str) -> String {
    line("home", &[quote(text)])
}

/// `exists POOL COUNT`
#[must_use]
pub fn exists(pool: &str, count: usize) -> String {
    line("exists", &[quote(pool), count.to_string()])
}

/// `created NAME ID STATE SECONDS EXIT OUTCOME ERROR`
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

/// `slot NAME ID STATE TASK RUNNING BUDGET BASE ARCH CREATED`
#[must_use]
pub fn slot(slot: &SlotStatus) -> String {
    let fields = [
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
        &[task, quote(&l.slot), quote(l.state), quote(&l.detail)],
    )
}

/// `collected TASK SLOT STATE DETAIL`
#[must_use]
pub fn collected(l: &RoundLine) -> String {
    round("collected", l)
}

/// `reconciled TASK SLOT STATE DETAIL`
#[must_use]
pub fn reconciled(l: &RoundLine) -> String {
    round("reconciled", l)
}

/// `nothing-open`
#[must_use]
pub fn nothing_open() -> String {
    "nothing-open".to_string()
}

/// `issue KIND SUBJECT MESSAGE FIRST CLEARED`
#[must_use]
pub fn issue(issue: &Issue) -> String {
    let fields = [
        quote(issue.kind),
        opt(issue.subject.as_deref()),
        quote(&issue.message),
        issue.first_seen.to_string(),
        num(issue.cleared),
    ];
    line("issue", &fields)
}

/// `check STATUS GROUP NAME DETAIL FIX MILLIS`
#[must_use]
pub fn check(check: &Check) -> String {
    let fields = [
        check.status.word().to_string(),
        quote(&check.group),
        quote(&check.name),
        quote(&check.detail),
        opt(check.fix.as_deref()),
        num(check.millis),
    ];
    line("check", &fields)
}

/// `summary OK WARN FAIL SKIPPED`
#[must_use]
pub fn summary(counts: &Counts) -> String {
    let fields = [
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
