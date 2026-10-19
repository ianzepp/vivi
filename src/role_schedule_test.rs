use super::*;
use chrono::TimeZone;

fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).unwrap()
}

fn signal(age_from_now: i64, now: DateTime<Utc>) -> LastSignal {
    LastSignal {
        at: now - chrono::Duration::seconds(age_from_now),
        handle: "abc".into(),
        local_role: "sent".into(),
    }
}

#[test]
fn auto_run_kind_flags_head_and_scoper_only() {
    assert!(auto_run_kind(Some("head")));
    assert!(auto_run_kind(Some("Head")));
    assert!(auto_run_kind(Some("HEAD")));
    assert!(auto_run_kind(Some(" scoper ")));
    assert!(!auto_run_kind(None));
    assert!(!auto_run_kind(Some("hand")));
    assert!(!auto_run_kind(Some("auditor")));
    assert!(!auto_run_kind(Some("mind")));
}

#[test]
fn none_without_cadence() {
    let report = evaluate(None, None, Utc::now());
    assert_eq!(report.state, ScheduleState::None);
    assert!(!report.action_required);
}

#[test]
fn never_with_cadence_and_no_signal() {
    let report = evaluate(Some("15m"), None, Utc::now());
    assert_eq!(report.state, ScheduleState::Never);
    assert_eq!(report.cadence.as_deref(), Some("15m"));
    assert_eq!(report.cadence_seconds, Some(900));
}

#[test]
fn ok_due_overdue_bands_with_ten_percent_grace() {
    let now = at(1_000_000);
    // cadence 100s → due after 110s, overdue at 200s
    let ok = evaluate(Some("100s"), Some(&signal(100, now)), now);
    assert_eq!(ok.state, ScheduleState::Ok);

    let still_ok = evaluate(Some("100s"), Some(&signal(109, now)), now);
    assert_eq!(still_ok.state, ScheduleState::Ok);

    let due = evaluate(Some("100s"), Some(&signal(110, now)), now);
    assert_eq!(due.state, ScheduleState::Due);
    assert!(!due.action_required);

    let still_due = evaluate(Some("100s"), Some(&signal(199, now)), now);
    assert_eq!(still_due.state, ScheduleState::Due);

    let overdue = evaluate(Some("100s"), Some(&signal(200, now)), now);
    assert_eq!(overdue.state, ScheduleState::Overdue);
    assert!(overdue.action_required);
    assert_eq!(overdue.age_seconds, Some(200));
}
