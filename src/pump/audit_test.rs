//! Tests for [`crate::pump::audit`]: the audit run round-trips through
//! `state.extra`, and the auditor brief carries the failings, the no-blame
//! ruling, the reply markers and any prior report.

use tempfile::tempdir;

use crate::pump::audit::{AUDIT_KEY, AuditRun, audit_brief, current, store};
use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::state::State;

#[test]
fn audit_run_round_trips_through_state_extra() {
    let dir = tempdir().unwrap();
    let (_ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let mut st = State::default();
    assert!(current(&st).is_none());
    let run = AuditRun {
        task: Some(String::from("task-1")),
        need: String::from("pumpfix-g01234567"),
        tip: String::from("abc1234"),
        pool: Some(String::from("here")),
        at: 1_000_000,
        fails: 2,
        retry_at: Some(1_000_060),
    };
    store(&mut st, Some(run.clone()));
    let got = current(&st).expect("stored audit run reads back");
    assert_eq!(got.task.as_deref(), Some("task-1"));
    assert_eq!(got.need, run.need);
    assert_eq!(got.tip, run.tip);
    assert_eq!(got.pool.as_deref(), Some("here"));
    assert_eq!(got.at, 1_000_000);
    assert_eq!(got.fails, 2);
    assert_eq!(got.retry_at, Some(1_000_060));
    assert!(st.extra.contains_key(AUDIT_KEY));
    store(&mut st, None);
    assert!(current(&st).is_none());
    assert!(!st.extra.contains_key(AUDIT_KEY));
}

#[test]
fn audit_brief_carries_failings_rules_and_prior() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let new = vec![
        String::from("gate::a_fails"),
        String::from("gate::b_fails"),
    ];
    let residual = vec![(String::from("gate::old_fails"), String::from("pumpfix-g1"))];
    let prior = "cause: off-by-one in x";
    let brief = audit_brief(&ctx, "abc1234", "test result: FAILED", &new, &residual, Some(prior));
    for name in ["gate::a_fails", "gate::b_fails", "gate::old_fails"] {
        assert!(brief.contains(name), "brief is missing {name}");
    }
    assert!(brief.contains("abc1234"));
    assert!(brief.contains("no suspect lists, no bisecting, no culprit commit"));
    assert!(brief.contains("never weaken, skip or delete tests"));
    assert!(brief.contains("Still failing after earlier fixes:"));
    assert!(brief.contains("Gate output (tail):"));
    assert!(brief.contains("test result: FAILED"));
    assert!(brief.contains("AUDIT-BEGIN"));
    assert!(brief.contains("AUDIT-END"));
    assert!(brief.contains("done_when:"));
    assert!(brief.contains("cause: off-by-one in x"));

    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let plain = audit_brief(&ctx, "abc1234", "test result: FAILED", &new, &[], None);
    assert!(!plain.contains("cause: off-by-one in x"));
    assert!(!plain.contains("Still failing after earlier fixes:"));
    assert!(!plain.contains("Prior audit report"));
    assert!(plain.contains("gate::a_fails"));
    assert!(plain.contains("AUDIT-BEGIN"));
}
