//! Doctor findings for the role roster, pid bindings, and head cadence.
//!
//! `findings` gathers from a live mailspace; `findings_from` is the pure,
//! tested core over prepared views, signals, and a probe closure.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use super::Finding;
use crate::mailspace::{Mailspace, RoleView};
use crate::role_schedule::{self, LastSignal, ScheduleReport, ScheduleState};
use crate::role_status::ProcessState;

/// Fix text for a head that needs waking.
const WAKE_FIX: &str = "wake the head (its charter's wake command)";

/// Gather role findings from a live mailspace.
///
/// Last signals are looked up exactly as `vivi role status` looks them up
/// (`Mailspace::schedule_report`), and liveness comes from the quick probe.
#[must_use]
pub fn findings(mailspace: &Mailspace, now: DateTime<Utc>) -> Vec<Finding> {
    let Ok(views) = mailspace.list_role_views() else {
        return vec![Finding::skipped("roles", "roster", "role roster unavailable")];
    };
    let mut last_signals = BTreeMap::new();
    for view in &views {
        if has_cadence(view) {
            last_signals.insert(view.name.clone(), latest_signal(mailspace, &view.name));
        }
    }
    findings_from(
        &views,
        &last_signals,
        &|view| {
            crate::role_status::probe_quick(
                view.pid,
                view.host.as_deref(),
                view.harness.as_deref(),
            )
            .state
        },
        now,
    )
}

/// Pure role findings from prepared views, last signals, and a probe closure.
#[must_use]
pub fn findings_from(
    views: &[RoleView],
    last_signals: &BTreeMap<String, Option<LastSignal>>,
    probe: &dyn Fn(&RoleView) -> ProcessState,
    now: DateTime<Utc>,
) -> Vec<Finding> {
    if views.is_empty() {
        return vec![Finding::skipped("roles", "roster", "no roles defined")];
    }
    let mut findings = vec![binding_finding(views, probe)];
    findings.extend(head_findings(views, last_signals, now));
    findings.extend(non_head_cadence_findings(views));
    findings
}

/// Whether the role carries a non-empty cadence (as `evaluate` sees it).
fn has_cadence(view: &RoleView) -> bool {
    view.cadence
        .as_deref()
        .is_some_and(|cadence| !cadence.trim().is_empty())
}

/// Look up a role's latest outbound signal like `vivi role status` does.
///
/// Storage or identity failures degrade to "no signal" rather than failing the
/// whole doctor run.
fn latest_signal(mailspace: &Mailspace, name: &str) -> Option<LastSignal> {
    let canonical = mailspace.resolve_identity(name).ok()?;
    let mut addresses: Vec<_> = mailspace
        .identity_names(&canonical)
        .into_iter()
        .flat_map(|identity| {
            [
                mailspace.address_for(&identity),
                format!("{identity}@{}", mailspace.legacy_domain()),
            ]
        })
        .collect();
    addresses.sort();
    addresses.dedup();
    let storage = mailspace.storage().ok()?;
    let message = storage.latest_message_from_addresses(&addresses).ok()??;
    Some(LastSignal {
        at: DateTime::parse_from_rfc3339(&message.date).map_or_else(
            |_| DateTime::from_timestamp(0, 0).unwrap_or_else(Utc::now),
            |d| d.with_timezone(&Utc),
        ),
        handle: message.handle,
        local_role: message.local_role,
    })
}

/// The one bindings finding: dead first, then zombie, else the alive count.
fn binding_finding(views: &[RoleView], probe: &dyn Fn(&RoleView) -> ProcessState) -> Finding {
    let mut dead = Vec::new();
    let mut zombie = Vec::new();
    let mut alive = Vec::new();
    let mut unchecked = Vec::new();
    for view in views {
        if view.pid.is_none() {
            continue;
        }
        match probe(view) {
            ProcessState::Dead => dead.push(&view.name),
            ProcessState::Zombie => zombie.push(&view.name),
            ProcessState::Alive => alive.push(&view.name),
            ProcessState::Remote
            | ProcessState::Subagent
            | ProcessState::Unknown
            | ProcessState::NotSet => unchecked.push(&view.name),
        }
    }
    if !dead.is_empty() {
        return Finding::warn(
            "roles",
            "bindings",
            &format!("dead binding: {}", names(dead.iter().map(|name| name.as_str()))),
            clear_pid_fix(&dead),
        );
    }
    if !zombie.is_empty() {
        return Finding::warn(
            "roles",
            "bindings",
            &format!("zombie: {}", names(zombie.iter().map(|name| name.as_str()))),
            clear_pid_fix(&zombie),
        );
    }
    let mut detail = format!("{} bound, all alive", alive.len());
    if !unchecked.is_empty() {
        detail.push_str(&format!(
            "; {} remote/subagent/unknown (neither alive nor dead)",
            unchecked.len()
        ));
    }
    Finding::ok("roles", "bindings", &detail)
}

/// One finding per cadence-bearing head that is due, overdue, or silent.
fn head_findings(
    views: &[RoleView],
    last_signals: &BTreeMap<String, Option<LastSignal>>,
    now: DateTime<Utc>,
) -> Vec<Finding> {
    let heads: Vec<&RoleView> = views
        .iter()
        .filter(|view| has_cadence(view) && view.kind.as_deref() == Some("head"))
        .collect();
    let mut findings = Vec::new();
    for head in &heads {
        let signal = last_signals.get(&head.name).and_then(|s| s.as_ref());
        let report = role_schedule::evaluate(head.cadence.as_deref(), signal, now);
        push_head_finding(&mut findings, &head.name, &report);
    }
    if findings.is_empty() && !heads.is_empty() {
        findings.push(Finding::ok("roles", "heads", "heads within cadence"));
    }
    findings
}

fn push_head_finding(findings: &mut Vec<Finding>, name: &str, report: &ScheduleReport) {
    match report.state {
        ScheduleState::Due => findings.push(Finding::warn(
            "roles",
            "heads",
            &format!("head {name} due"),
            WAKE_FIX,
        )),
        ScheduleState::Overdue => findings.push(Finding::fail(
            "roles",
            "heads",
            &format!("head {name} overdue ({})", age_label(report.age_seconds)),
            WAKE_FIX,
        )),
        ScheduleState::Never => findings.push(Finding::warn(
            "roles",
            "heads",
            &format!("head {name} has never signalled"),
            WAKE_FIX,
        )),
        ScheduleState::Ok | ScheduleState::None => {}
    }
}

/// One aggregated warning per non-head kind that carries a cadence.
fn non_head_cadence_findings(views: &[RoleView]) -> Vec<Finding> {
    let mut by_kind: BTreeMap<&str, Vec<&RoleView>> = BTreeMap::new();
    for view in views {
        if has_cadence(view) && view.kind.as_deref() != Some("head") {
            by_kind
                .entry(view.kind.as_deref().unwrap_or("unset"))
                .or_default()
                .push(view);
        }
    }
    by_kind
        .into_iter()
        .map(|(kind, group)| {
            Finding::warn(
                "roles",
                "non-head cadence",
                &format!(
                    "cadence set on non-head kind {kind}: {}",
                    names(group.iter().map(|view| view.name.as_str()))
                ),
                cadence_fix(&group),
            )
        })
        .collect()
}

fn names<'a, I>(values: I) -> String
where
    I: IntoIterator<Item = &'a str>,
{
    values.into_iter().collect::<Vec<_>>().join(", ")
}

fn clear_pid_fix(names_list: &[&String]) -> String {
    names_list
        .iter()
        .map(|name| format!("vivi role set {name} --clear-pid"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn cadence_fix(group: &[&RoleView]) -> String {
    group
        .iter()
        .map(|view| {
            let cadence = view.cadence.as_deref().unwrap_or_default();
            format!("vivi role set {} --cadence {cadence} review or clear", view.name)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Compact human label for a signal age, e.g. `2d3h`, `45m`, `30s`.
fn age_label(age_seconds: Option<u64>) -> String {
    let Some(secs) = age_seconds else {
        return "unknown age".to_string();
    };
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let minutes = (secs % 3_600) / 60;
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
#[path = "roles_section_test.rs"]
mod tests;
