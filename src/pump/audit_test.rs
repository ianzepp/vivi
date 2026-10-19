//! Tests for [`crate::pump::audit`]: audit state round-trips through `extra`
//! and the auditor brief carries failures, rules, and any prior report.

use std::fs;

use tempfile::tempdir;

use crate::pump::audit::{
    AuditRun, Cause, apply_report, audit_brief, current, parse_report, store,
};
use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::state::{State, Status};

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
    assert_eq!(current(&st), Some(run));

    store(&mut st, None);
    assert!(current(&st).is_none());
}

#[test]
fn audit_brief_carries_failings_rules_and_prior() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let new = vec![String::from("gate::a_fails"), String::from("gate::b_fails")];
    let residual = vec![(String::from("gate::old_fails"), String::from("pumpfix-g1"))];
    let prior = "cause: off-by-one in x";
    let brief = audit_brief(
        &ctx,
        "abc1234",
        "test result: FAILED",
        &new,
        &residual,
        Some(prior),
    );

    for name in ["gate::a_fails", "gate::b_fails", "gate::old_fails"] {
        assert!(brief.contains(name), "brief is missing {name}");
    }
    assert!(brief.contains("abc1234"));
    assert!(brief.contains("no suspect lists, no bisecting, no culprit commit"));
    assert!(brief.contains("It must never weaken, skip or delete tests."));
    assert!(brief.contains("Still failing after earlier fixes:"));
    assert!(brief.contains("Gate output (tail):"));
    assert!(brief.contains("test result: FAILED"));
    assert!(brief.contains("AUDIT-BEGIN"));
    assert!(brief.contains("AUDIT-END"));
    assert!(brief.contains("done_when:"));
    assert!(brief.contains("cause: off-by-one in x"));

    let plain = audit_brief(&ctx, "abc1234", "test result: FAILED", &new, &[], None);
    assert!(!plain.contains("cause: off-by-one in x"));
    assert!(!plain.contains("Still failing after earlier fixes:"));
    assert!(!plain.contains("Prior audit report"));
    assert!(plain.contains("gate::a_fails"));
    assert!(plain.contains("AUDIT-BEGIN"));
}

#[test]
fn parse_report_reads_the_last_marker_block() {
    let first = r#"[{"root_cause": "stale cause", "brief": "old brief",
        "tests": ["old::t"], "write_scope": ["src/old.rs"]}]"#;
    let second = r#"[
  {
    "root_cause": "gate counts merges, not landed batches",
    "brief": "Replace the counter check with a merge-batch check in start_gate.",
    "tests": ["gate::a_fails", "gate::b_fails"],
    "write_scope": ["src/pump/gate.rs"],
    "proof": ["cargo test --lib pump::gate_test"]
  },
  {
    "root_cause": "audit retries never dispatch",
    "brief": "Re-arm the retry timer in start_audit.",
    "tests": [],
    "write_scope": ["src/pump/audit.rs", "src/pump/audit_test.rs"],
    "proof": []
  }
]"#;
    let text = format!(
        "preamble\nAUDIT-BEGIN\n{first}\nAUDIT-END\nsome prose\
         \nAUDIT-BEGIN\n{second}\nAUDIT-END\ntrailer\n"
    );

    let causes = parse_report(&text).unwrap();
    assert_eq!(causes.len(), 2);
    assert_eq!(
        causes[0].root_cause,
        "gate counts merges, not landed batches"
    );
    assert_eq!(
        causes[0].brief,
        "Replace the counter check with a merge-batch check in start_gate."
    );
    assert_eq!(causes[0].tests, ["gate::a_fails", "gate::b_fails"]);
    assert_eq!(causes[0].write_scope, ["src/pump/gate.rs"]);
    assert_eq!(causes[0].proof, ["cargo test --lib pump::gate_test"]);
    assert_eq!(causes[1].tests, [] as [String; 0]);
    assert_eq!(
        causes[1].write_scope,
        ["src/pump/audit.rs", "src/pump/audit_test.rs"]
    );
}

#[test]
fn parse_report_rejects_missing_markers_bad_json_and_empty_scopes() {
    // No markers at all.
    assert!(parse_report("the reply never carried a report").is_none());
    // A BEGIN with no later END is no complete block.
    assert!(parse_report("AUDIT-BEGIN\n[]\n").is_none());
    // A block body that is not JSON.
    assert!(parse_report("AUDIT-BEGIN\nnot json\nAUDIT-END\n").is_none());
    // An empty array.
    assert!(parse_report("AUDIT-BEGIN\n[]\nAUDIT-END\n").is_none());
    // A cause with an empty write scope.
    let empty_scope = r#"[{"root_cause": "x", "brief": "y", "write_scope": []}]"#;
    assert!(parse_report(&format!("AUDIT-BEGIN\n{empty_scope}\nAUDIT-END\n")).is_none());
}

#[test]
fn apply_report_mints_one_unit_per_cause() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let mut st = State::default();
    let long_root_cause = "audit retries stop after the timer expires because the \
        fails counter is never reset on a good run and the timer is never re-armed";
    assert!(long_root_cause.chars().count() > 90);
    let causes = vec![
        Cause {
            root_cause: String::from("gate counts merges, not landed batches"),
            brief: String::from("Replace the counter check with a merge-batch check."),
            tests: vec![String::from("gate::a_fails")],
            write_scope: vec![String::from("src/pump/gate.rs")],
            proof: vec![String::from("cargo test --lib pump::gate_test")],
        },
        Cause {
            root_cause: long_root_cause.to_string(),
            brief: String::from("Re-arm the retry timer in start_audit."),
            tests: Vec::new(),
            write_scope: vec![String::from("src/pump/audit.rs")],
            proof: Vec::new(),
        },
    ];
    let raw = "AUDIT-BEGIN\n[]\nAUDIT-END\n";

    let n = apply_report(&ctx, &mut st, "pumpfix-g1", raw, &causes);

    assert_eq!(n, 2);
    assert_eq!(
        st.order,
        [String::from("pumpfix-g1-1"), String::from("pumpfix-g1-2")]
    );
    let first = &st.units["pumpfix-g1-1"];
    assert_eq!(first.status, Status::Pending);
    assert_eq!(first.need, "pumpfix-g1");
    assert_eq!(first.title, "gate counts merges, not landed batches");
    assert_eq!(first.write_scope, ["src/pump/gate.rs"]);
    let second = &st.units["pumpfix-g1-2"];
    assert_eq!(second.status, Status::Pending);
    assert_eq!(
        second.title,
        long_root_cause.chars().take(90).collect::<String>()
    );
    assert_eq!(second.write_scope, ["src/pump/audit.rs"]);

    let record = &st.needs["pumpfix-g1"];
    assert_eq!(record.verdict, "units");
    assert_eq!(record.reason, "audit report");
    assert_eq!(
        record.units,
        [String::from("pumpfix-g1-1"), String::from("pumpfix-g1-2")]
    );
    assert_eq!(record.revision, 1);

    let report = fs::read_to_string(ctx.home.join("audit").join("pumpfix-g1.report")).unwrap();
    assert_eq!(report, raw);
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("audit report for pumpfix-g1: 2 fix units"))
    );
}
