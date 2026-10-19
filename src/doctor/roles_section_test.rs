//! Tests for the doctor roles section.

use std::collections::BTreeMap;

use chrono::{Duration, TimeZone, Utc};
use tempfile::tempdir;

use super::Finding;
use super::{findings, findings_from};
use crate::doctor::Status;
use crate::mailspace::{Mailspace, RoleView};
use crate::role_schedule::LastSignal;
use crate::role_status::ProcessState;

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 19, 15, 0, 0).unwrap()
}

fn view(name: &str, kind: Option<&str>, cadence: Option<&str>, pid: Option<u32>) -> RoleView {
    RoleView {
        name: name.to_string(),
        address: format!("{name}@test.local"),
        aliases: Vec::new(),
        kind: kind.map(str::to_string),
        labels: Vec::new(),
        status: "open".to_string(),
        harness: None,
        provider: None,
        model: None,
        thinking: None,
        pid,
        host: None,
        cadence: cadence.map(str::to_string),
        pool: None,
        read_only: None,
        models: Vec::new(),
        charter: String::new(),
        has_charter: false,
    }
}

fn signal_ago(seconds: i64) -> LastSignal {
    LastSignal {
        at: now() - Duration::seconds(seconds),
        handle: "msg-1".to_string(),
        local_role: "hand".to_string(),
    }
}

fn signals(entries: &[(&str, LastSignal)]) -> BTreeMap<String, Option<LastSignal>> {
    entries
        .iter()
        .map(|(name, signal)| ((*name).to_string(), Some(signal.clone())))
        .collect()
}

fn no_signals() -> BTreeMap<String, Option<LastSignal>> {
    BTreeMap::new()
}

fn never_bound(_view: &RoleView) -> ProcessState {
    ProcessState::NotSet
}

#[test]
fn due_head_warns_and_overdue_head_fails() {
    let views = vec![
        view("mind", Some("head"), Some("1h"), None),
        view("late", Some("head"), Some("1h"), None),
    ];
    let last_signals = signals(&[("mind", signal_ago(90 * 60)), ("late", signal_ago(3 * 60 * 60))]);
    let result = findings_from(&views, &last_signals, &never_bound, now());

    let due = result
        .iter()
        .find(|finding| finding.detail == "head mind due")
        .unwrap_or_else(|| panic!("due head finding missing in {result:?}"));
    assert_eq!(due.status, Status::Warn);
    assert_eq!(due.fix.as_deref(), Some("wake the head (its charter's wake command)"));

    let overdue = result
        .iter()
        .find(|finding| finding.detail.starts_with("head late overdue ("))
        .unwrap_or_else(|| panic!("overdue head finding missing in {result:?}"));
    assert_eq!(overdue.status, Status::Fail);
    assert_eq!(overdue.fix.as_deref(), Some("wake the head (its charter's wake command)"));

    assert!(result.iter().all(|finding| finding.section == "roles"));
}

#[test]
fn dead_binding_warns_with_clear_pid_fix() {
    let views = vec![
        view("mind", None, None, Some(42)),
        view("hand", None, None, Some(7)),
    ];
    let result = findings_from(&views, &no_signals(), &|_view| ProcessState::Dead, now());

    let binding = result
        .iter()
        .find(|finding| finding.name == "bindings")
        .unwrap_or_else(|| panic!("bindings finding missing in {result:?}"));
    assert_eq!(binding.status, Status::Warn);
    assert_eq!(binding.detail, "dead binding: mind, hand");
    assert_eq!(
        binding.fix.as_deref(),
        Some("vivi role set mind --clear-pid; vivi role set hand --clear-pid")
    );
}

#[test]
fn cadence_on_non_head_warns() {
    let views = vec![view("bench", Some("bench"), Some("30m"), None)];
    let result = findings_from(&views, &no_signals(), &never_bound, now());

    let offender = result
        .iter()
        .find(|finding| finding.name == "non-head cadence")
        .unwrap_or_else(|| panic!("non-head cadence finding missing in {result:?}"));
    assert_eq!(offender.status, Status::Warn);
    assert_eq!(offender.detail, "cadence set on non-head kind bench: bench");
    assert_eq!(
        offender.fix.as_deref(),
        Some("vivi role set bench --cadence 30m review or clear")
    );

    // No heads exist, so no "heads within cadence" summary is emitted.
    assert!(result.iter().all(|finding| finding.name != "heads"));
}

#[test]
fn empty_roster_skips() {
    let result = findings_from(&[], &no_signals(), &never_bound, now());
    assert_eq!(
        result,
        vec![Finding::skipped("roles", "roster", "no roles defined")]
    );
}

#[test]
fn gather_on_fresh_mailspace_skips() {
    let tmp = tempdir().unwrap();
    let mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    let result = findings(&mailspace, Utc::now());
    assert_eq!(
        result,
        vec![Finding::skipped("roles", "roster", "no roles defined")]
    );
}
