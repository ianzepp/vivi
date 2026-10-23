//! Stall alarms: the pump noticing that work is delivered but not moving.
//!
//! A person should not have to read `log.txt` to learn that delivered work
//! sits unmerged. [`check`] files two exceptions (`stall-merge`, and
//! `blocking-stuck` from [`crate::pump::blockers`]), each at most once per
//! subject per hour; [`status_lines`] adds the delivered backlog to
//! `status.txt`. See skills/vivi/references/pump-contract.md, section "Stall
//! alarms".

use serde_json::{Map, Value, json};

use crate::pump::host::Ctx;
use crate::pump::state::{State, Status};

/// Stall alarm thresholds (the `[pump.alarm]` table).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AlarmConfig {
    /// Minutes units may sit `delivered` with nothing merging before a
    /// `stall-merge` exception.
    pub merge_idle_minutes: u64,
    /// Units a blocked root must hold, directly or transitively, before the
    /// pump reports it as a blocker.
    pub blocking_hold_units: usize,
    /// Minutes a blocker may hold its units before it is re-planned.
    pub blocking_replan_minutes: u64,
}

impl Default for AlarmConfig {
    fn default() -> Self {
        Self {
            merge_idle_minutes: 45,
            blocking_hold_units: 10,
            blocking_replan_minutes: 120,
        }
    }
}

/// Key in `State::extra` holding `{ "<kind> <unit>": <epoch seconds raised> }`.
const RAISED_KEY: &str = "alarm_raised";
/// A subject is raised at most once per this many seconds.
const REPEAT_SECS: u64 = 3600;
/// Raised-time records older than this are dropped.
const FORGET_SECS: u64 = 86_400;

/// One exception about to be filed.
pub(crate) struct Finding {
    pub(crate) kind: &'static str,
    pub(crate) unit: String,
    pub(crate) text: String,
}

/// Raise every due stall exception. Each subject (`<kind> <unit>`) is filed
/// at most once per hour; the raised times live in `st.extra["alarm_raised"]`
/// so they survive a restart. Call it on the top-level state, outside
/// `with_target_state`, so `extra` is the workspace one.
pub fn check(ctx: &Ctx, st: &mut State) {
    let now = ctx.now();
    let mut findings = merge_findings(ctx, st, now);
    findings.extend(crate::pump::blockers::findings(ctx, st, now));
    let raised = raised_map(st);
    raised.retain(|_, at| {
        at.as_u64()
            .is_some_and(|at| now.saturating_sub(at) < FORGET_SECS)
    });
    for finding in findings {
        let subject = format!("{} {}", finding.kind, finding.unit);
        let raised = raised_map(st);
        let recent = raised
            .get(&subject)
            .and_then(Value::as_u64)
            .is_some_and(|at| now.saturating_sub(at) < REPEAT_SECS);
        if recent {
            continue;
        }
        raised.insert(subject, json!(now));
        ctx.exception(finding.kind, &finding.unit, &finding.text);
    }
}

/// The line `status.txt` carries for the stall signal: the delivered backlog.
#[must_use]
pub fn status_lines(ctx: &Ctx, st: &State) -> Vec<String> {
    let now = ctx.now();
    let oldest = delivered_since(st).into_iter().min();
    let delivered = oldest.map_or_else(
        || String::from("delivered waiting 0, oldest none"),
        |since| {
            format!(
                "delivered waiting {}, oldest {}",
                delivered_since(st).len(),
                age(now.saturating_sub(since))
            )
        },
    );
    vec![delivered]
}

/// The mutable raised-time map, created or repaired on demand.
fn raised_map(st: &mut State) -> &mut Map<String, Value> {
    let slot = st
        .extra
        .entry(String::from(RAISED_KEY))
        .or_insert_with(|| json!({}));
    if !slot.is_object() {
        *slot = json!({});
    }
    match slot {
        Value::Object(map) => map,
        _ => unreachable!("alarm_raised was just made an object"),
    }
}

/// `since` of every `delivered` unit.
fn delivered_since(st: &State) -> Vec<u64> {
    st.units
        .values()
        .filter(|unit| unit.status == Status::Delivered)
        .map(|unit| unit.since)
        .collect()
}

/// Units delivered while nothing has merged for `alarm.merge_idle_minutes`.
fn merge_findings(ctx: &Ctx, st: &State, now: u64) -> Vec<Finding> {
    let limit = ctx.cfg.alarm.merge_idle_minutes.saturating_mul(60);
    let delivered = delivered_since(st);
    let Some(oldest) = delivered.iter().copied().min() else {
        return Vec::new();
    };
    let last_merge = st.units.values().filter_map(|unit| unit.merged_at).max();
    let idle_since = last_merge.unwrap_or(oldest);
    if now.saturating_sub(oldest) <= limit || now.saturating_sub(idle_since) <= limit {
        return Vec::new();
    }
    let merged = last_merge.map_or_else(
        || String::from("nothing has merged"),
        |at| format!("last merge {} ago", age(now.saturating_sub(at))),
    );
    vec![Finding {
        kind: "stall-merge",
        unit: String::from("pump"),
        text: format!(
            "{} delivered, oldest waiting {}; {merged} (limit {} minutes)",
            delivered.len(),
            age(now.saturating_sub(oldest)),
            ctx.cfg.alarm.merge_idle_minutes
        ),
    }]
}

/// `2h05m` from an hour up, else `14m`.
pub(crate) fn age(secs: u64) -> String {
    let minutes = secs / 60;
    if minutes >= 60 {
        format!("{}h{:02}m", minutes / 60, minutes % 60)
    } else {
        format!("{minutes}m")
    }
}
