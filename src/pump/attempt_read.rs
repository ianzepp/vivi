//! Reading attempt records back: the reverse of
//! [`crate::pump::attempt_record`], and the timing summary a need's close
//! line carries.
//!
//! Two texts on a need thread describe a hand attempt: the machine-readable
//! `key: value` block that opens an attempt record, and the pool's result
//! reply (`pool result <job>: <word>` / `pool <p>  slot <s>  model <m>` /
//! `agent exit <n> in <m>s ... started from <epoch>`). Both parse to the same
//! [`AttemptFields`]. A separate status event can enrich it with later
//! lifecycle timestamps after the attempt has been posted.
//!
//! Pure string parsing: no mailspace, no store, no clock. Callers pass the
//! epoch they mean by "now".

use chrono::DateTime;

/// The first line of an attempt record.
const HEADER: &str = "pump: attempt record";
/// The first line of an attempt status event.
const STATUS_HEADER: &str = "pump: attempt status event";
/// The opening of a pool result reply.
const POOL_RESULT: &str = "pool result ";
/// The pool summary fragment carrying the agent's exit code.
const AGENT_EXIT: &str = "agent exit ";
/// The pool summary fragment carrying the dispatch epoch.
const STARTED: &str = "started from ";
/// The word a block writes for an absent value.
const ABSENT: &str = "none";
/// The keys of the block that opens an attempt record, in written order.
const KEYS: [&str; 16] = [
    "attempt",
    "need",
    "task",
    "actor",
    "role",
    "kind",
    "pool",
    "slot",
    "model",
    "ready-at",
    "start",
    "end",
    "agent-seconds",
    "delivered-at",
    "merged-at",
    "outcome",
];
/// Keys accepted in an attempt status event.
const STATUS_KEYS: [&str; 4] = ["attempt", "need", "status", "at"];
/// The units [`human_secs`] writes, largest first.
const UNITS: [(&str, u64); 4] = [("d", 86_400), ("h", 3_600), ("m", 60), ("s", 1)];

/// What one attempt record or pool result reply says about the attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AttemptFields {
    /// The attempt handle (`<unit>-a<n>`) or the pool job id.
    #[serde(default)]
    pub attempt: String,
    /// The need thread the attempt belongs to.
    #[serde(default)]
    pub need: Option<String>,
    /// The dispatch task.
    #[serde(default)]
    pub task: Option<String>,
    /// Who ran the attempt.
    #[serde(default)]
    pub actor: Option<String>,
    /// The role the actor played.
    #[serde(default)]
    pub role: Option<String>,
    /// The pool the attempt ran in.
    #[serde(default)]
    pub pool: Option<String>,
    /// The pool slot the attempt held.
    #[serde(default)]
    pub slot: Option<String>,
    /// The model the attempt ran on.
    #[serde(default)]
    pub model: Option<String>,
    /// The configured dispatch kind.
    #[serde(default)]
    pub kind: Option<String>,
    /// Unix epoch seconds when the unit first became dispatch-ready.
    #[serde(default)]
    pub ready_at: Option<u64>,
    /// The dispatch epoch, in seconds.
    #[serde(default)]
    pub start: Option<u64>,
    /// The end epoch, in seconds.
    #[serde(default)]
    pub end: Option<u64>,
    /// The agent's own wall-clock seconds.
    #[serde(default)]
    pub agent_seconds: Option<u64>,
    /// Unix epoch seconds when the unit was delivered to the merge queue.
    #[serde(default)]
    pub delivered_at: Option<u64>,
    /// Unix epoch seconds when the attempt merged, joined from a status event.
    #[serde(default)]
    pub merged_at: Option<u64>,
    /// How the attempt ended (`delivered`, `proof-failed`, ...).
    #[serde(default)]
    pub outcome: String,
}

impl AttemptFields {
    /// Enrich this record from a matching synced lifecycle event.
    pub fn apply_status_event(&mut self, event: &AttemptStatusEvent) -> bool {
        let need_mismatch = self
            .need
            .as_ref()
            .zip(event.need.as_ref())
            .is_some_and(|(record, status)| record != status);
        if self.attempt != event.attempt || need_mismatch {
            return false;
        }
        match event.status.as_str() {
            "delivered" if event.at.is_some() => self.delivered_at = event.at,
            "merged" if event.at.is_some() => self.merged_at = event.at,
            "delivered" | "merged" => {}
            _ => return false,
        }
        true
    }
}

impl AttemptFields {
    /// Store one block line: an absent or blank value leaves the field empty.
    fn set(&mut self, key: &str, value: &str) {
        match key {
            "attempt" => self.attempt = required(value),
            "need" => self.need = optional(value),
            "task" => self.task = optional(value),
            "actor" => self.actor = optional(value),
            "role" => self.role = optional(value),
            "pool" => self.pool = optional(value),
            "slot" => self.slot = optional(value),
            "model" => self.model = optional(value),
            "kind" => self.kind = optional(value),
            "ready-at" => self.ready_at = secs(value),
            "start" => self.start = secs(value),
            "end" => self.end = secs(value),
            "agent-seconds" => self.agent_seconds = secs(value),
            "delivered-at" => self.delivered_at = secs(value),
            "merged-at" => self.merged_at = secs(value),
            "outcome" => self.outcome = required(value),
            _ => {}
        }
    }
}

/// Parse the `key: value` block that opens an attempt record. The first line
/// must be exactly `pump: attempt record`; block lines are read while their
/// key is one of [`KEYS`], so the prose `unit:` line ends the block. An
/// attempt or an outcome that is missing (or `none`) means this is not a
/// record.
#[must_use]
pub fn parse_record(body: &str) -> Option<AttemptFields> {
    let mut lines = body.lines();
    if lines.next()?.trim_end() != HEADER {
        return None;
    }
    let mut fields = AttemptFields::default();
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            break;
        };
        let key = key.trim();
        if !KEYS.contains(&key) {
            break;
        }
        fields.set(key, value.trim());
    }
    if fields.attempt.is_empty() || fields.outcome.is_empty() {
        return None;
    }
    Some(fields)
}

/// A synced lifecycle event that enriches one attempt record.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AttemptStatusEvent {
    /// The attempt handle (`<unit>-a<n>`).
    #[serde(default)]
    pub attempt: String,
    /// The need thread carrying the event.
    #[serde(default)]
    pub need: Option<String>,
    /// The lifecycle status (`merged`, ...).
    #[serde(default)]
    pub status: String,
    /// Unix epoch seconds when this status was recorded.
    #[serde(default)]
    pub at: Option<u64>,
}

/// Parse a synced attempt status event. Missing timestamps remain unavailable.
#[must_use]
pub fn parse_status_event(body: &str) -> Option<AttemptStatusEvent> {
    let mut lines = body.lines();
    if lines.next()?.trim_end() != STATUS_HEADER {
        return None;
    }
    let mut event = AttemptStatusEvent::default();
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            break;
        };
        let key = key.trim();
        if !STATUS_KEYS.contains(&key) {
            break;
        }
        let value = value.trim();
        match key {
            "attempt" => event.attempt = required(value),
            "need" => event.need = optional(value),
            "status" => event.status = required(value),
            "at" => event.at = secs(value),
            _ => {}
        }
    }
    if event.attempt.is_empty() || event.status.is_empty() {
        return None;
    }
    Some(event)
}

/// Parse a pool result reply on a need thread. The result line names the job
/// and the outcome word, the summary line names the pool, slot and model, and
/// the `agent exit` line carries the timing.
#[must_use]
pub fn parse_receipt(body: &str) -> Option<AttemptFields> {
    if !body.contains(POOL_RESULT) || !body.contains(AGENT_EXIT) {
        return None;
    }
    let mut fields = AttemptFields::default();
    let result = body.lines().find(|line| line.contains(POOL_RESULT))?;
    let (_, rest) = result.split_once(POOL_RESULT)?;
    let (job, rest) = rest.split_once(':')?;
    fields.attempt = job.trim().to_owned();
    fields.outcome = rest
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    if let Some(line) = body.lines().find(|line| summary_line(line)) {
        read_summary(&mut fields, line);
    }
    fields.start = body.split_once(STARTED).and_then(|(_, rest)| digits(rest));
    fields.agent_seconds = exit_secs(body);
    if fields.attempt.is_empty() || fields.outcome.is_empty() {
        return None;
    }
    Some(fields)
}

/// Whether `line` is the pool's `pool <p>  slot <s>  model <m>` summary.
fn summary_line(line: &str) -> bool {
    line.starts_with("pool ") && line.contains("slot ") && !line.contains(POOL_RESULT)
}

/// Read the pool, slot and model of a summary line, leaving the rest alone.
fn read_summary(fields: &mut AttemptFields, line: &str) {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    for pair in tokens.chunks(2) {
        let [key, value] = pair else {
            continue;
        };
        fields.set(key, value);
    }
}

/// The agent's wall-clock seconds in a pool summary: `agent exit <n> in
/// <m>s`, or `agent exit <n> after <m>s`.
fn exit_secs(body: &str) -> Option<u64> {
    let (_, rest) = body.split_once(AGENT_EXIT)?;
    let rest = rest
        .trim_start()
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '-');
    [" in ", " after "]
        .into_iter()
        .find_map(|marker| rest.strip_prefix(marker))
        .and_then(digits)
}

/// An RFC 3339 timestamp as epoch seconds.
#[must_use]
pub fn opened_epoch(date: &str) -> Option<u64> {
    let when = match DateTime::parse_from_rfc3339(date.trim()) {
        Ok(when) => when,
        Err(_) => return None,
    };
    match u64::try_from(when.timestamp()) {
        Ok(epoch) => Some(epoch),
        Err(_) => None,
    }
}

/// Seconds in the two most significant nonzero units: `1h2m`, `1d2h`, `12s`,
/// `0s`.
#[must_use]
pub fn human_secs(secs: u64) -> String {
    let mut out = String::new();
    let mut left = secs;
    let mut shown = 0;
    for (label, size) in UNITS {
        if shown == 2 {
            break;
        }
        let count = left / size;
        if count == 0 {
            continue;
        }
        left %= size;
        out.push_str(&count.to_string());
        out.push_str(label);
        shown += 1;
    }
    if out.is_empty() {
        out.push_str("0s");
    }
    out
}

/// The close-line timing of a need: open-to-done, how many attempts, the
/// summed agent time, and the slowest attempt.
#[must_use]
pub fn timing_summary(opened: Option<u64>, attempts: &[AttemptFields], now: u64) -> String {
    let done = opened.map_or_else(
        || String::from("unknown"),
        |at| human_secs(now.saturating_sub(at)),
    );
    let total: u64 = attempts
        .iter()
        .map(|attempt| attempt.agent_seconds.unwrap_or_default())
        .sum();
    let slowest = attempts
        .iter()
        .filter_map(|attempt| {
            attempt
                .agent_seconds
                .map(|secs| (secs, attempt.attempt.as_str()))
        })
        .max_by_key(|(secs, _)| *secs);
    format!(
        "timing: open-to-done {done}; attempts {}; agent time {}; {}",
        attempts.len(),
        human_secs(total),
        slowest_text(slowest),
    )
}

/// The `slowest <attempt> (<time>)` tail, or `slowest none`.
fn slowest_text(slowest: Option<(u64, &str)>) -> String {
    match slowest {
        Some((secs, attempt)) => format!("slowest {attempt} ({})", human_secs(secs)),
        None => String::from("slowest none"),
    }
}

/// An optional string: `none` and blank are absent.
fn optional(value: &str) -> Option<String> {
    if value.is_empty() || value == ABSENT {
        return None;
    }
    Some(value.to_owned())
}

/// A required string: empty when the value says `none`.
fn required(value: &str) -> String {
    optional(value).unwrap_or_default()
}

/// A decimal number, or absent for `none` and anything unparsable.
fn secs(value: &str) -> Option<u64> {
    match value.parse() {
        Ok(number) => Some(number),
        Err(_) => None,
    }
}

/// The decimal number that opens `text`, when there is one.
fn digits(text: &str) -> Option<u64> {
    let head: String = text.chars().take_while(char::is_ascii_digit).collect();
    secs(&head)
}
