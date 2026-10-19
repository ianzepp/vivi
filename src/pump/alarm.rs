//! Stall alarms: the pump noticing that work is delivered but not moving.
//!
//! A person should not have to read `log.txt` to learn that fix units sit
//! unmerged or that `main` has not been promoted for hours. [`check`] files
//! four exceptions (`stall-fix-unit`, `stall-promote`, `stall-merge`, and
//! `blocking-stuck` from [`crate::pump::blockers`]), each at most once per
//! subject per hour; [`status_lines`] adds the same facts to `status.txt`. See
//! skills/vivi/references/pump-contract.md, section "Stall alarms".

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::pump::host::Ctx;
use crate::pump::state::{GateRecord, Promotion, State, Status};

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

/// One repository target's promotion history.
struct Scope<'a> {
    id: String,
    promotions: &'a [Promotion],
    gates: &'a [GateRecord],
    baseline: Option<&'a [String]>,
    red: &'a BTreeMap<String, String>,
}

/// Raise every due stall exception. Each subject (`<kind> <unit>`) is filed
/// at most once per hour; the raised times live in `st.extra["alarm_raised"]`
/// so they survive a restart. Call it on the top-level state, outside
/// `with_target_state`, so `extra` is the workspace one.
pub fn check(ctx: &Ctx, st: &mut State) {
    let now = ctx.now();
    let mut findings = fix_unit_findings(ctx, st, now);
    findings.extend(promote_findings(ctx, st, now));
    findings.extend(merge_findings(ctx, st, now));
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

/// The lines `status.txt` carries for the stall signals: last promotion, last
/// green gate, oldest waiting fix unit, and the delivered backlog.
#[must_use]
pub fn status_lines(ctx: &Ctx, st: &State) -> Vec<String> {
    let now = ctx.now();
    let promote = scopes(ctx, st)
        .iter()
        .filter_map(|scope| scope.promotions.last())
        .max_by_key(|promotion| promotion.at)
        .map_or_else(
            || String::from("last promote never"),
            |promotion| {
                format!(
                    "last promote {} ago ({})",
                    age(now.saturating_sub(promotion.at)),
                    tip10(&promotion.tip)
                )
            },
        );
    let green = scopes(ctx, st)
        .iter()
        .filter_map(|scope| {
            crate::pump::gate::green::green_gate(scope.gates, scope.promotions, scope.baseline)
        })
        .max_by_key(|gate| gate.at)
        .map_or_else(
            || String::from("last green never"),
            |gate| {
                format!(
                    "last green {} ago ({})",
                    age(now.saturating_sub(gate.at)),
                    tip10(&gate.tip)
                )
            },
        );
    let fix = waiting_fix_units(ctx, st).into_iter().next().map_or_else(
        || String::from("oldest waiting fix none"),
        |(uid, since)| {
            format!(
                "oldest waiting fix {uid} {}",
                age(now.saturating_sub(since))
            )
        },
    );
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
    vec![promote, green, fix, delivered]
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

/// Fix units in `delivered` or `merging`, oldest first, as `(uid, since)`.
fn waiting_fix_units(ctx: &Ctx, st: &State) -> Vec<(String, u64)> {
    let prefix = ctx.cfg.fix_prefix.as_str();
    let mut waiting: Vec<(String, u64)> = st
        .units
        .iter()
        .filter(|(uid, unit)| {
            uid.starts_with(prefix) && matches!(unit.status, Status::Delivered | Status::Merging)
        })
        .map(|(uid, unit)| (uid.clone(), unit.since))
        .collect();
    waiting.sort_by_key(|(_, since)| *since);
    waiting
}

/// `since` of every `delivered` unit.
fn delivered_since(st: &State) -> Vec<u64> {
    st.units
        .values()
        .filter(|unit| unit.status == Status::Delivered)
        .map(|unit| unit.since)
        .collect()
}

/// A fix unit waiting past `alarm.fix_wait_minutes`.
fn fix_unit_findings(ctx: &Ctx, st: &State, now: u64) -> Vec<Finding> {
    let limit = ctx.cfg.alarm.fix_wait_minutes.saturating_mul(60);
    waiting_fix_units(ctx, st)
        .into_iter()
        .filter(|(_, since)| now.saturating_sub(*since) > limit)
        .map(|(uid, since)| {
            let status = st
                .units
                .get(&uid)
                .map_or("delivered", |u| u.status.as_str());
            Finding {
                kind: "stall-fix-unit",
                text: format!(
                    "fix unit {status} for {} (limit {} minutes)",
                    age(now.saturating_sub(since)),
                    ctx.cfg.alarm.fix_wait_minutes
                ),
                unit: uid,
            }
        })
        .collect()
}

/// Every configured target with its promotion and gate history.
fn scopes<'a>(ctx: &Ctx, st: &'a State) -> Vec<Scope<'a>> {
    ctx.cfg
        .target_configs()
        .into_keys()
        .filter_map(|id| {
            if id == "default" {
                return Some(Scope {
                    id,
                    promotions: &st.promotions,
                    gates: &st.gates,
                    baseline: st.baseline.as_deref(),
                    red: &st.red,
                });
            }
            let target = st.target_states.get(&id)?;
            Some(Scope {
                id,
                promotions: &target.promotions,
                gates: &target.gates,
                baseline: target.baseline.as_deref(),
                red: &target.red,
            })
        })
        .collect()
}

/// Merges landed since the last promotion without one following.
fn promote_findings(ctx: &Ctx, st: &State, now: u64) -> Vec<Finding> {
    let limit = ctx.cfg.alarm.promote_hours.saturating_mul(3600);
    let mut out = Vec::new();
    for (id, target) in ctx.cfg.target_configs() {
        if !target.promote || target.main.is_empty() {
            continue;
        }
        let Some(scope) = scopes(ctx, st).into_iter().find(|scope| scope.id == id) else {
            continue;
        };
        let last = scope.promotions.last();
        let floor = last.map_or(0, |promotion| promotion.at);
        let merged: Vec<u64> = st
            .units
            .values()
            .filter(|unit| unit.target_id() == id)
            .filter_map(|unit| unit.merged_at)
            .filter(|at| *at > floor)
            .collect();
        let Some(oldest) = merged.iter().copied().min() else {
            continue;
        };
        let since = last.map_or(oldest, |promotion| promotion.at);
        if now.saturating_sub(since) <= limit {
            continue;
        }
        let blockers = scope
            .gates
            .last()
            .and_then(|gate| blockers_text(&scope, gate));
        out.push(Finding {
            kind: "stall-promote",
            text: promote_text(now, last, merged.len(), scope.gates, blockers),
            unit: id,
        });
    }
    out
}

/// The last gate's failing names that main does not carry, with the fix unit
/// owning each. `None` when the gate is unreadable or nothing blocks it.
fn blockers_text(scope: &Scope, gate: &GateRecord) -> Option<String> {
    let names = gate.names.as_ref()?;
    let main = crate::pump::gate::green::main_set(scope.promotions, scope.gates, scope.baseline);
    let mut off_main: Vec<&String> = names.iter().filter(|name| !main.contains(name)).collect();
    off_main.sort();
    off_main.dedup();
    if off_main.is_empty() {
        return None;
    }
    let shown = off_main.iter().take(5).map(|name| {
        scope.red.get(*name).map_or_else(
            || format!("{name} (no fix)"),
            |owner| format!("{name} (fix {owner})"),
        )
    });
    let shown: Vec<String> = shown.collect();
    let more = off_main.len().saturating_sub(shown.len());
    let tail = if more > 0 {
        format!(" and {more} more")
    } else {
        String::new()
    };
    Some(format!(
        "{} reds not on main: {}{tail}",
        off_main.len(),
        shown.join(", ")
    ))
}

/// The `stall-promote` text: hours since promotion, merges waiting, last gate.
fn promote_text(
    now: u64,
    last: Option<&Promotion>,
    merged: usize,
    gates: &[GateRecord],
    blockers: Option<String>,
) -> String {
    let since = last.map_or_else(
        || String::from("no promotion yet"),
        |promotion| {
            format!(
                "{} since last promotion",
                age(now.saturating_sub(promotion.at))
            )
        },
    );
    let gate = gates.last().map_or_else(
        || String::from("no gate has finished"),
        |gate| {
            let failing = gate.names.as_ref().map_or_else(
                || String::from("unreadable"),
                |names| names.len().to_string(),
            );
            format!("last gate {} failing {failing}", tip10(&gate.tip))
        },
    );
    let base = format!("{since}; {merged} merged awaiting promotion; {gate}");
    blockers.map_or(base.clone(), |blockers| format!("{base}; {blockers}"))
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

/// The first ten characters of a tip.
fn tip10(tip: &str) -> String {
    tip.chars().take(10).collect()
}
