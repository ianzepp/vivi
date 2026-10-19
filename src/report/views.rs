//! Fixed calculations over report records. Missing source fields stay null.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::model::{AttemptRecord, Dimension, Filter, NeedWindow, ReportData, View};

const RECORD_COLUMNS: &[&str] = &[
    "need",
    "task",
    "attempt",
    "actor",
    "role",
    "pool",
    "slot",
    "model",
    "kind",
    "start",
    "end",
    "agent_seconds",
    "outcome",
    "ready_at",
    "delivered_at",
    "merged_at",
];
const THROUGHPUT_COLUMNS: &[&str] = &[
    "group",
    "dispatched",
    "delivered",
    "merged",
    "failed",
    "unavailable",
];
const DURATION_COLUMNS: &[&str] = &[
    "group",
    "measure",
    "samples",
    "unavailable",
    "median_seconds",
    "p90_seconds",
    "max_seconds",
];
const NEED_COLUMNS: &[&str] = &[
    "group",
    "needs",
    "open_needs",
    "lead_seconds",
    "unavailable_lead_times",
    "effort_seconds",
    "unavailable_agent_seconds",
    "attempts",
];
const FAILURE_COLUMNS: &[&str] = &["group", "count", "repeat_units"];

#[must_use]
pub fn columns(view: View) -> &'static [&'static str] {
    match view {
        View::Records => RECORD_COLUMNS,
        View::Throughput => THROUGHPUT_COLUMNS,
        View::Durations => DURATION_COLUMNS,
        View::Needs => NEED_COLUMNS,
        View::Failures => FAILURE_COLUMNS,
    }
}

#[must_use]
pub fn rows(data: &ReportData, view: View, filter: &Filter) -> Vec<Value> {
    let selected = selected_attempts(data, filter);
    let mut rows = match view {
        View::Records => record_rows(&time_filtered(&selected, filter, |item| item.start)),
        View::Throughput => throughput_rows(&selected, filter),
        View::Durations => {
            duration_rows(&time_filtered(&selected, filter, |item| item.start), filter)
        }
        View::Needs => need_rows(data, &selected, filter),
        View::Failures => failure_rows(&time_filtered(&selected, filter, |item| item.end), filter),
    };
    apply_top(&mut rows, view, filter.top);
    rows
}

fn selected_attempts<'a>(data: &'a ReportData, filter: &Filter) -> Vec<&'a AttemptRecord> {
    data.attempts
        .iter()
        .filter(|attempt| {
            filter
                .need
                .as_deref()
                .is_none_or(|value| attempt.need == value)
        })
        .filter(|attempt| {
            filter
                .actor
                .as_deref()
                .is_none_or(|value| attempt.actor.as_deref() == Some(value))
        })
        .filter(|attempt| {
            filter
                .model
                .as_deref()
                .is_none_or(|value| attempt.model.as_deref() == Some(value))
        })
        .collect()
}

fn time_filtered<'a>(
    attempts: &[&'a AttemptRecord],
    filter: &Filter,
    time: impl Fn(&AttemptRecord) -> Option<u64>,
) -> Vec<&'a AttemptRecord> {
    attempts
        .iter()
        .copied()
        .filter(|attempt| in_window(time(attempt), filter))
        .collect()
}

fn in_window(time: Option<u64>, filter: &Filter) -> bool {
    if filter.since.is_none() && filter.until.is_none() {
        return true;
    }
    time.is_some_and(|time| {
        filter.since.is_none_or(|since| time >= since)
            && filter.until.is_none_or(|until| time <= until)
    })
}

fn record_rows(attempts: &[&AttemptRecord]) -> Vec<Value> {
    attempts.iter().map(|attempt| json!(attempt)).collect()
}

#[derive(Default)]
struct Counts {
    dispatched: u64,
    delivered: u64,
    merged: u64,
    failed: u64,
    missing_dispatched: u64,
    missing_delivered: u64,
    missing_merged: u64,
    missing_failed: u64,
}

fn throughput_rows(attempts: &[&AttemptRecord], filter: &Filter) -> Vec<Value> {
    let dimension = filter.by.unwrap_or(Dimension::Day);
    let mut groups: BTreeMap<String, Counts> = BTreeMap::new();
    for attempt in attempts {
        if in_window(attempt.start, filter) {
            let count = groups
                .entry(key(attempt, dimension, attempt.start))
                .or_default();
            if attempt.start.is_some() {
                count.dispatched += 1;
            } else {
                count.missing_dispatched += 1;
            }
        }
        count_delivery(attempt, dimension, &mut groups, filter);
        count_merge(attempt, dimension, &mut groups, filter);
        count_failure(attempt, dimension, &mut groups, filter);
    }
    groups
        .into_iter()
        .map(|(group, counts)| json!({
            "group": group,
            "dispatched": known_count(counts.dispatched, counts.missing_dispatched),
            "delivered": known_count(counts.delivered, counts.missing_delivered),
            "merged": known_count(counts.merged, counts.missing_merged),
            "failed": known_count(counts.failed, counts.missing_failed),
            "unavailable": counts.missing_dispatched + counts.missing_delivered + counts.missing_merged + counts.missing_failed,
        }))
        .collect()
}

fn count_delivery(
    attempt: &AttemptRecord,
    dimension: Dimension,
    groups: &mut BTreeMap<String, Counts>,
    filter: &Filter,
) {
    if attempt.outcome != "delivered" {
        return;
    }
    if !in_window(attempt.delivered_at, filter) {
        return;
    }
    let counts = groups
        .entry(key(attempt, dimension, attempt.delivered_at))
        .or_default();
    if attempt.delivered_at.is_some() {
        counts.delivered += 1;
    } else {
        counts.missing_delivered += 1;
    }
}

fn count_merge(
    attempt: &AttemptRecord,
    dimension: Dimension,
    groups: &mut BTreeMap<String, Counts>,
    filter: &Filter,
) {
    if let Some(merged_at) = attempt.merged_at {
        if !in_window(Some(merged_at), filter) {
            return;
        }
        groups
            .entry(key(attempt, dimension, Some(merged_at)))
            .or_default()
            .merged += 1;
    } else if attempt.outcome == "delivered"
        && attempt.merged_at.is_none()
        && in_window(None, filter)
    {
        groups
            .entry(key(attempt, dimension, None))
            .or_default()
            .missing_merged += 1;
    }
}

fn count_failure(
    attempt: &AttemptRecord,
    dimension: Dimension,
    groups: &mut BTreeMap<String, Counts>,
    filter: &Filter,
) {
    if is_success(&attempt.outcome) {
        return;
    }
    if !in_window(attempt.end, filter) {
        return;
    }
    let counts = groups
        .entry(key(attempt, dimension, attempt.end))
        .or_default();
    if attempt.end.is_some() {
        counts.failed += 1;
    } else {
        counts.missing_failed += 1;
    }
}

fn known_count(count: u64, missing: u64) -> Option<u64> {
    (missing == 0).then_some(count)
}

fn duration_rows(attempts: &[&AttemptRecord], filter: &Filter) -> Vec<Value> {
    let dimension = filter.by.unwrap_or(Dimension::Need);
    let measures = ["wait_while_ready", "agent_time", "delivered_to_merged"];
    let mut groups: BTreeMap<(String, String), (Vec<u64>, u64)> = BTreeMap::new();
    for attempt in attempts {
        let group = key(attempt, dimension, attempt.start);
        let durations = [
            attempt
                .start
                .zip(attempt.ready_at)
                .and_then(|(start, ready)| start.checked_sub(ready)),
            attempt.agent_seconds,
            attempt
                .merged_at
                .zip(attempt.delivered_at)
                .and_then(|(merged, delivered)| merged.checked_sub(delivered)),
        ];
        for (measure, duration) in measures.iter().zip(durations) {
            let entry = groups
                .entry((group.clone(), (*measure).into()))
                .or_default();
            if let Some(duration) = duration {
                entry.0.push(duration);
            } else if missing_is_unavailable(attempt, measure) {
                entry.1 += 1;
            }
        }
    }
    groups
        .into_iter()
        .map(|((group, measure), (mut values, unavailable))| {
            values.sort_unstable();
            json!({
                "group": group,
                "measure": measure,
                "samples": values.len(),
                "unavailable": unavailable,
                "median_seconds": percentile_median(&values),
                "p90_seconds": percentile_90(&values),
                "max_seconds": values.last().copied(),
            })
        })
        .collect()
}

fn missing_is_unavailable(attempt: &AttemptRecord, measure: &str) -> bool {
    match measure {
        "wait_while_ready" => attempt
            .ready_at
            .zip(attempt.start)
            .and_then(|(ready, start)| start.checked_sub(ready))
            .is_none(),
        "agent_time" => attempt.agent_seconds.is_none(),
        "delivered_to_merged" => {
            attempt.outcome == "delivered"
                && attempt
                    .delivered_at
                    .zip(attempt.merged_at)
                    .and_then(|(delivered, merged)| merged.checked_sub(delivered))
                    .is_none()
        }
        _ => false,
    }
}

fn percentile_median(values: &[u64]) -> Option<u64> {
    match values.len() {
        0 => None,
        n if n % 2 == 1 => values.get(n / 2).copied(),
        n => {
            let left = values[n / 2 - 1];
            let right = values[n / 2];
            Some(left / 2 + right / 2 + (left % 2 + right % 2) / 2)
        }
    }
}

fn percentile_90(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let rank = (values.len() * 9).div_ceil(10).saturating_sub(1);
    values.get(rank).copied()
}

fn need_rows(data: &ReportData, attempts: &[&AttemptRecord], filter: &Filter) -> Vec<Value> {
    need_groups(data, attempts, filter)
        .into_iter()
        .map(|(group, result)| need_row(&group, result))
        .collect()
}

#[derive(Default)]
struct NeedGroup {
    leads: Vec<u64>,
    unavailable_leads: usize,
    open_needs: usize,
    efforts: Vec<Option<u64>>,
    attempts: usize,
    needs: usize,
}

fn need_groups(
    data: &ReportData,
    attempts: &[&AttemptRecord],
    filter: &Filter,
) -> BTreeMap<String, NeedGroup> {
    let windows: Vec<&NeedWindow> = data
        .needs
        .iter()
        .filter(|need| {
            filter
                .need
                .as_deref()
                .is_none_or(|value| need.need == value)
                && in_window(need.done_at.or(need.opened_at), filter)
        })
        .collect();
    let mut groups: BTreeMap<String, NeedGroup> = BTreeMap::new();
    for window in windows {
        let related: Vec<&AttemptRecord> = attempts
            .iter()
            .copied()
            .filter(|attempt| attempt.need == window.need)
            .collect();
        if related.is_empty() && (filter.actor.is_some() || filter.model.is_some()) {
            continue;
        }
        let group_time = window.done_at.or(window.opened_at);
        let group = filter.by.map_or_else(
            || window.need.clone(),
            |dimension| {
                related.first().map_or_else(
                    || "unavailable".into(),
                    |attempt| key(attempt, dimension, group_time),
                )
            },
        );
        let entry = groups.entry(group).or_default();
        if !window.done {
            entry.open_needs += 1;
        } else if let Some(lead) = window
            .done_at
            .zip(window.opened_at)
            .and_then(|(done, opened)| done.checked_sub(opened))
        {
            entry.leads.push(lead);
        } else {
            entry.unavailable_leads += 1;
        }
        entry.needs += 1;
        entry.attempts += related.len();
        for attempt in related {
            entry.efforts.push(attempt.agent_seconds);
        }
    }
    groups
}

fn need_row(group: &str, result: NeedGroup) -> Value {
    let known_effort: Vec<u64> = result.efforts.iter().flatten().copied().collect();
    json!({
        "group": group,
        "needs": result.needs,
        "open_needs": result.open_needs,
        "lead_seconds": (result.unavailable_leads == 0).then(|| average(&result.leads)).flatten(),
        "unavailable_lead_times": result.unavailable_leads,
        "effort_seconds": match (result.attempts, known_effort.len() == result.efforts.len()) {
            (0, _) => Some(0),
            (_, true) => checked_sum(&known_effort),
            _ => None,
        },
        "unavailable_agent_seconds": result.efforts.iter().filter(|value| value.is_none()).count(),
        "attempts": result.attempts,
    })
}

fn failure_rows(attempts: &[&AttemptRecord], filter: &Filter) -> Vec<Value> {
    let mut groups: BTreeMap<String, Vec<&AttemptRecord>> = BTreeMap::new();
    for attempt in attempts
        .iter()
        .copied()
        .filter(|attempt| !is_success(&attempt.outcome))
    {
        let group = filter.by.map_or_else(
            || attempt.outcome.clone(),
            |dimension| key(attempt, dimension, attempt.end),
        );
        groups.entry(group).or_default().push(attempt);
    }
    groups
        .into_iter()
        .map(|(group, attempts)| {
            let mut by_unit: BTreeMap<String, usize> = BTreeMap::new();
            for attempt in &attempts {
                *by_unit.entry(unit_id(&attempt.attempt)).or_default() += 1;
            }
            let repeats: Vec<String> = by_unit
                .into_iter()
                .filter_map(|(unit, count)| (count > 1).then_some(unit))
                .collect();
            json!({ "group": group, "count": attempts.len(), "repeat_units": repeats })
        })
        .collect()
}

fn is_success(outcome: &str) -> bool {
    matches!(outcome, "delivered" | "no-defect")
}

fn unit_id(attempt: &str) -> String {
    attempt
        .rsplit_once("-a")
        .map_or(attempt, |(unit, _)| unit)
        .to_owned()
}

fn key(attempt: &AttemptRecord, dimension: Dimension, time: Option<u64>) -> String {
    match dimension {
        Dimension::Actor => value(&attempt.actor),
        Dimension::Role => value(&attempt.role),
        Dimension::Model => value(&attempt.model),
        Dimension::Pool => value(&attempt.pool),
        Dimension::Kind => value(&attempt.kind),
        Dimension::Need => attempt.need.clone(),
        Dimension::Day => bucket(time, "%Y-%m-%d"),
        Dimension::Hour => bucket(time, "%Y-%m-%dT%H:00Z"),
    }
}

fn value(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "unavailable".into())
}

fn average(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let sum: u128 = values.iter().map(|value| u128::from(*value)).sum();
    match u64::try_from(sum / values.len() as u128) {
        Ok(average) => Some(average),
        Err(_) => None,
    }
}

fn checked_sum(values: &[u64]) -> Option<u64> {
    values
        .iter()
        .try_fold(0_u64, |sum, value| sum.checked_add(*value))
}

fn bucket(time: Option<u64>, format: &str) -> String {
    time.and_then(|time| match i64::try_from(time) {
        Ok(time) => Some(time),
        Err(_) => None,
    })
    .and_then(|time| DateTime::<Utc>::from_timestamp(time, 0))
    .map_or_else(
        || "unavailable".into(),
        |time| time.format(format).to_string(),
    )
}

fn apply_top(rows: &mut Vec<Value>, view: View, top: Option<usize>) {
    let Some(top) = top else { return };
    let metric = match view {
        View::Throughput => "dispatched",
        View::Durations => "samples",
        View::Needs => "attempts",
        View::Failures => "count",
        View::Records => return,
    };
    rows.sort_by_key(|row| std::cmp::Reverse(row[metric].as_u64().unwrap_or_default()));
    rows.truncate(top);
}
