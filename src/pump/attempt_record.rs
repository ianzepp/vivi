//! Attempt records: the pump's account of one hand attempt, posted as a
//! `pump.note = "attempt"` reply on the slice task when the attempt ends.
//!
//! The reply names the unit, attempt, model, pool, how it ended, the branch,
//! the task, the failing command with a short excerpt, and the hand's own
//! final message. The hand's final message comes from the task receipt
//! (`vivi task show <task> --latest`), so it works for remote pools whose hands
//! cannot reach the mailspace. Posting never blocks or fails the tick: one try
//! per attempt end, a failed send is logged and dropped.

use crate::pool::record::{SECTION_RECEIPT, SECTION_RUNNER_LOG, SECTION_STDERR, split_sections};
use crate::pump::collect::tail;
use crate::pump::host::Ctx;
use crate::pump::jobs::PoolJob;
use crate::pump::record::reply_on_task;

/// Characters of the hand's final message kept (the tail).
const FINAL_MESSAGE_CHARS: usize = 1500;
/// Characters of a failure excerpt kept (the tail).
const EXCERPT_CHARS: usize = 800;
/// The pool summary line that carries the agent's exit code.
const EXIT_MARKER: &str = "agent exit ";
/// The pool summary fragment that carries the dispatch time.
const STARTED_MARKER: &str = "started from ";

/// The failing command of an attempt and a short excerpt of its output.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Failing {
    /// The command that failed, when there is one.
    command: Option<String>,
    /// The tail of what it printed.
    excerpt: String,
}

/// How one attempt ended, as far as the record is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ending {
    /// The ending in words (`delivered`, `proof-failed`, ...).
    label: String,
    /// The failing command and excerpt, for a failure.
    failing: Option<Failing>,
    /// The task's full receipt when the caller already read it.
    receipt: Option<String>,
}

impl Ending {
    /// A failed attempt: `why` is the short reason (`proof-failed`,
    /// `merge-conflict`, `<state>/<detail>` of an agent that failed, ...) and
    /// `detail` the failure text.
    #[must_use]
    pub fn failed(why: &str, detail: &str, receipt: Option<&str>) -> Self {
        let merge_stage = matches!(
            why,
            "proof-failed" | "merge-conflict" | "branch-missing" | "timeout"
        );
        if merge_stage {
            return Self::of(why, Some(stage_failure(why, detail)), receipt);
        }
        let known = receipt.map_or_else(|| detail.to_owned(), str::to_owned);
        let parsed = parse_receipt(&known);
        let command = parsed.exit.map(|code| format!("agent exit {code}"));
        let failing = (command.is_some() || !parsed.stderr.is_empty()).then_some(Failing {
            command,
            excerpt: tail(&parsed.stderr, EXCERPT_CHARS),
        });
        Self::of(&format!("agent failed: {why}"), failing, receipt)
    }

    fn of(label: &str, failing: Option<Failing>, receipt: Option<&str>) -> Self {
        Self {
            label: label.to_owned(),
            failing,
            receipt: receipt.map(str::to_owned),
        }
    }
}

/// The failing command and excerpt of a merge-stage failure. A proof failure
/// carries "this command failed (exit N):\n<command>\n\n<heading>:\n<output>".
fn stage_failure(why: &str, detail: &str) -> Failing {
    if why == "proof-failed"
        && let Some((_, rest)) = detail.split_once("this command failed")
        && let Some((_, rest)) = rest.split_once('\n')
        && let Some((command, output)) = rest.split_once("\n\n")
    {
        let output = output.split_once(":\n").map_or(output, |(_, text)| text);
        return Failing {
            command: Some(command.trim().to_owned()),
            excerpt: tail(output.trim_end(), EXCERPT_CHARS),
        };
    }
    Failing {
        command: None,
        excerpt: tail(detail.trim_end(), EXCERPT_CHARS),
    }
}

/// The parts of a task receipt a record uses.
#[derive(Debug, Default, PartialEq, Eq)]
struct Parsed {
    /// The agent's exit code, from the pool's summary line.
    exit: Option<i32>,
    /// The hand's own final message.
    message: String,
    /// The stderr tail the pool collected.
    stderr: String,
}

/// The agent's exit code in a task receipt, when its summary line is there.
#[must_use]
pub fn agent_exit(receipt: &str) -> Option<i32> {
    let (_, rest) = receipt.split_once(EXIT_MARKER)?;
    leading_number(rest, |c| c.is_ascii_digit() || c == '-')
}

/// The number written by the leading run of characters `keep` accepts.
fn leading_number<T: std::str::FromStr>(text: &str, keep: impl Fn(char) -> bool) -> Option<T> {
    let digits: String = text.chars().take_while(|c| keep(*c)).collect();
    match digits.parse() {
        Ok(number) => Some(number),
        Err(_) => None,
    }
}

/// The agent's wall-clock seconds in a task receipt's summary line: the
/// number after `agent exit <n> in <m>s` or `agent exit <n> after <m>s`.
#[must_use]
pub fn agent_secs(text: &str) -> Option<u64> {
    let (_, rest) = text.split_once(EXIT_MARKER)?;
    let rest = rest
        .trim_start()
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '-');
    let after = [" in ", " after "]
        .into_iter()
        .find_map(|m| rest.strip_prefix(m))?;
    leading_number(after, |c| c.is_ascii_digit())
}

/// The dispatch time in a task receipt's summary line: the epoch seconds
/// after `started from `.
fn started_from(text: &str) -> Option<u64> {
    let (_, rest) = text.split_once(STARTED_MARKER)?;
    leading_number(rest, |c| c.is_ascii_digit())
}

/// The joined text of the stderr and runner-log tail sections of a task
/// receipt: evidence scans that must not read the agent's own message
/// section read this instead of the whole receipt.
#[must_use]
pub(crate) fn sections_tail(text: &str) -> String {
    let mut joined = String::new();
    for (title, body) in split_sections(text).1 {
        if matches!(title, SECTION_STDERR | SECTION_RUNNER_LOG) {
            if !joined.is_empty() {
                joined.push('\n');
            }
            joined.push_str(body.trim());
        }
    }
    joined
}

/// The hand's final message from a full pool task receipt.
#[must_use]
pub fn hand_message(text: &str) -> String {
    parse_receipt(text).message
}

/// Split a task receipt: the pool's summary lines come first, then the
/// rendered sections. A receipt with no `--- receipt` section is taken as
/// the hand's message whole.
fn parse_receipt(text: &str) -> Parsed {
    let mut parsed = Parsed {
        exit: agent_exit(text),
        ..Parsed::default()
    };
    let (head, sections) = split_sections(text);
    let mut found = false;
    for (title, body) in sections {
        match title {
            SECTION_RECEIPT => {
                parsed.message = body.trim().to_owned();
                found = true;
            }
            SECTION_STDERR => parsed.stderr = body.trim().to_owned(),
            _ => {}
        }
    }
    if !found {
        parsed.message = head.trim().to_owned();
    }
    parsed
}

/// Everything a record says.
struct Facts {
    task: String,
    actor: String,
    role: String,
    start: Option<u64>,
    end: u64,
    agent_secs: Option<u64>,
    attempt: u32,
    model: String,
    pool: String,
    target: String,
    branch: Option<String>,
    label: String,
    failing: Option<Failing>,
    message: String,
}

/// Post the record of one attempt as a `pump.note = "attempt"` reply on the slice
/// task itself, from the pump identity, so the next attempt's brief carries
/// it. `facts_text` is the task receipt; the ending's own receipt wins when
/// it has one. Returns whether the reply went out; a failure is logged and
/// never blocks the caller.
pub fn post_on_task(
    ctx: &Ctx,
    task: &str,
    job: &PoolJob,
    ending: &Ending,
    facts_text: &str,
) -> bool {
    let receipt = ending.receipt.as_deref().unwrap_or(facts_text);
    let facts = Facts {
        task: task.to_owned(),
        actor: ctx.actor.clone(),
        role: ctx.cfg.to.clone(),
        start: started_from(receipt),
        end: ctx.now(),
        agent_secs: agent_secs(receipt),
        attempt: job
            .payload
            .get("attempt")
            .and_then(serde_json::Value::as_u64)
            .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX)),
        model: job.model.clone().unwrap_or_else(|| "None".to_owned()),
        pool: job.pool.clone().unwrap_or_else(|| "?".to_owned()),
        target: job.target.clone().unwrap_or_default(),
        branch: job.branch.clone(),
        label: ending.label.clone(),
        failing: ending.failing.clone(),
        message: tail(&parse_receipt(receipt).message, FINAL_MESSAGE_CHARS),
    };
    let body = match crate::mailspace::pump_note::note_body(
        crate::letter::records::NoteWord::Attempt,
        Some(&ending.label),
        &compose(&facts),
    ) {
        Ok(body) => body,
        Err(err) => {
            ctx.log(&format!(
                "attempt record render failed on task {task}: {err}"
            ));
            return false;
        }
    };
    let sent = reply_on_task(
        ctx,
        task,
        task,
        &format!("attempt-{}", slug(&ending.label)),
        &body,
    );
    if !sent {
        ctx.log(&format!("attempt record send failed on task {task}"));
    }
    sent
}

/// A file-name-safe form of `label`.
fn slug(label: &str) -> String {
    let word: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    word.trim_matches('-').chars().take(24).collect()
}

/// The text of the record.
fn compose(facts: &Facts) -> String {
    let mut out = String::from("pump: attempt record\n");
    out.push_str(&header_block(facts));
    out.push_str(&format!(
        "unit: {}  attempt: {}  model: {}  pool: {}  target: {}\n",
        facts.task, facts.attempt, facts.model, facts.pool, facts.target
    ));
    out.push_str(&format!(
        "branch: {} (none: the hand left no changes)\n",
        facts.branch.as_deref().unwrap_or("none")
    ));
    out.push_str(&format!("task: {}\nbase: \n", facts.task));
    if let Some(failing) = &facts.failing {
        out.push_str(&failing_block(failing));
    }
    out.push_str("hand's final message:\n");
    out.push_str(&indent(&facts.message, "(none)"));
    out
}

/// The machine-readable `key: value` block that opens every record: one
/// line per fact, `none` for absent values, decimal epochs.
fn header_block(facts: &Facts) -> String {
    format!(
        "attempt: {task}-a{attempt}\n\
         need: {need}\n\
         task: {need}\n\
         actor: {actor}\n\
         role: {role}\n\
         kind: none\n\
         pool: {pool}\n\
         slot: none\n\
         model: {model}\n\
         ready-at: none\n\
         start: {start}\n\
         end: {end}\n\
         agent-seconds: {secs}\n\
         delivered-at: none\n\
         merged-at: none\n\
         outcome: {outcome}\n",
        task = facts.task,
        attempt = facts.attempt,
        need = or_none(&facts.task),
        actor = or_none(&facts.actor),
        role = or_none(&facts.role),
        pool = or_none(&facts.pool),
        model = or_none(&facts.model),
        start = num(facts.start),
        end = facts.end,
        secs = num(facts.agent_secs),
        outcome = facts.label,
    )
}

/// A header value: `none` when it is blank.
fn or_none(value: &str) -> &str {
    if value.trim().is_empty() {
        "none"
    } else {
        value
    }
}

/// A header number: decimal, or `none` when it is absent.
fn num(value: Option<u64>) -> String {
    value.map_or_else(|| String::from("none"), |n| n.to_string())
}

/// The failing command and its excerpt.
fn failing_block(failing: &Failing) -> String {
    let mut out = String::from("failing command:\n");
    out.push_str(&indent(failing.command.as_deref().unwrap_or(""), "(none)"));
    if !failing.excerpt.is_empty() {
        out.push_str("excerpt:\n");
        out.push_str(&indent(&failing.excerpt, ""));
    }
    out
}

/// `text` indented two spaces per line, `empty` when it has no text; ends
/// with a newline.
fn indent(text: &str, empty: &str) -> String {
    if text.trim().is_empty() {
        return if empty.is_empty() {
            String::new()
        } else {
            format!("  {empty}\n")
        };
    }
    text.lines().map(|line| format!("  {line}\n")).collect()
}
