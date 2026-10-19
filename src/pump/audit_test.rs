//! Tests for [`crate::pump::audit`]: audit state round-trips through `extra`
//! and the auditor brief carries failures, rules, and any prior report.

use std::collections::BTreeMap;
use std::fs;

use tempfile::tempdir;

use crate::pump::audit::{
    AuditRun, Cause, apply_report, audit_brief, current, parse_report, start_audit, store,
};
use crate::pump::config::{GateConfig, PumpConfig};
use crate::pump::gatefix::fix_need;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{State, Status, Unit};
use crate::routing::{RoleRouting, RoutedRole};

/// A 40-hex integration tip; its first eight characters name the fix need.
const TIP: &str = "0123456789abcdef0123456789abcdef01234567";

/// Pump config with one gate pool, the gate model and a gate budget.
fn audit_cfg() -> PumpConfig {
    PumpConfig {
        gate: GateConfig {
            pools: vec![String::from("apool")],
            model: String::from("gatemodel"),
            budget: 600,
            ..GateConfig::default()
        },
        ..PumpConfig::default()
    }
}

/// Routing with one auditor role that supplies its own pool.
fn auditor_routing() -> RoleRouting {
    RoleRouting {
        roles: vec![RoutedRole {
            name: String::from("auditor"),
            kind: Some(String::from("auditor")),
            status: String::from("active"),
            pool: Some(String::from("apool")),
            read_only: None,
            model: None,
            models: Vec::new(),
        }],
        pools: BTreeMap::new(),
    }
}

/// The vivi replies an audit start needs: a free slot, a task handle and a
/// dispatch receipt.
fn ready(host: &FakeHost) {
    host.reply(
        "pool status --porcelain",
        0,
        "slot apool bridge slot-1 free",
    );
    host.reply("task send", 0, "created 1234abcd");
    host.reply("pool dispatch", 0, "dispatched 1234abcd to apool");
}

/// The `pool dispatch` command line an audit start ran.
fn dispatch_command(host: &FakeHost) -> String {
    host.commands()
        .into_iter()
        .find(|cmd| cmd.contains("pool dispatch"))
        .expect("no pool dispatch command ran")
}

/// A context with [`audit_cfg`] and its fake host.
fn setup(routing: Option<RoleRouting>) -> (tempfile::TempDir, Ctx, std::sync::Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), audit_cfg());
    let ctx = match routing {
        Some(routing) => ctx.with_routing(routing),
        None => ctx,
    };
    (dir, ctx, host)
}

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
    let earlier = r#"[{"root_cause":"old cause","brief":"old brief","write_scope":["old.rs"]}]"#;
    let latest = r#"[
        {
            "root_cause": "root cause two",
            "brief": "concrete fix for the failing behavior",
            "tests": ["pump::audit_test::first", "pump::audit_test::second"],
            "write_scope": ["src/pump/audit.rs"],
            "proof": ["cargo test --lib pump::audit"]
        }
    ]"#;
    let text = format!(
        "preamble\nAUDIT-BEGIN\n{earlier}\nAUDIT-END\nextra prose\n\
         AUDIT-BEGIN\n{latest}\nAUDIT-END\ntrailer"
    );

    let causes = parse_report(&text).expect("the final report block should parse");
    assert_eq!(causes.len(), 1);
    assert_eq!(causes[0].root_cause, "root cause two");
    assert_eq!(causes[0].brief, "concrete fix for the failing behavior");
    assert_eq!(
        causes[0].tests,
        ["pump::audit_test::first", "pump::audit_test::second"]
    );
    assert_eq!(causes[0].write_scope, ["src/pump/audit.rs"]);
    assert_eq!(causes[0].proof, ["cargo test --lib pump::audit"]);
}

#[test]
fn parse_report_rejects_missing_markers_bad_json_and_empty_scopes() {
    assert!(parse_report("plain reply without markers").is_none());
    assert!(parse_report("AUDIT-BEGIN\nnot json\nAUDIT-END").is_none());
    assert!(parse_report("AUDIT-BEGIN\n[]\nAUDIT-END").is_none());
    let empty_scope = r#"[{"root_cause":"broken","brief":"fix it","write_scope":[]}]"#;
    let empty_scope_report = format!("AUDIT-BEGIN\n{empty_scope}\nAUDIT-END");
    assert!(parse_report(&empty_scope_report).is_none());
}

#[test]
fn apply_report_mints_one_unit_per_cause() {
    let (_dir, ctx, host) = setup(None);
    let mut st = State::default();
    let causes = vec![
        Cause {
            root_cause: String::from("first root cause"),
            brief: String::from("fix the first cause"),
            tests: vec![String::from("pump::first")],
            write_scope: vec![String::from("src/first.rs")],
            proof: vec![String::from("cargo test --lib pump::first")],
        },
        Cause {
            root_cause: String::from("second root cause"),
            brief: String::from("fix the second cause"),
            tests: vec![String::from("pump::second")],
            write_scope: vec![String::from("src/second.rs")],
            proof: vec![String::from("cargo test --lib pump::second")],
        },
    ];
    let raw = "AUDIT-BEGIN\nraw report\nAUDIT-END\n";

    assert_eq!(apply_report(&ctx, &mut st, "need-audit", raw, &causes), 2);
    assert_eq!(st.order, ["need-audit-1", "need-audit-2"]);
    for uid in &st.order {
        assert_eq!(st.units[uid].status, Status::Pending);
        assert_eq!(st.units[uid].need, "need-audit");
    }
    assert_eq!(st.units["need-audit-1"].title, "first root cause");
    assert_eq!(st.units["need-audit-1"].brief, "fix the first cause");
    assert_eq!(st.units["need-audit-1"].write_scope, ["src/first.rs"]);
    assert_eq!(st.units["need-audit-1"].proof, ["cargo test --lib pump::first"]);
    assert_eq!(st.units["need-audit-1"].tools, "");

    let record = &st.needs["need-audit"];
    assert_eq!(record.verdict, "units");
    assert_eq!(record.reason, "audit report");
    assert_eq!(record.units, ["need-audit-1", "need-audit-2"]);
    assert_eq!(record.revision, 1);
    assert_eq!(
        fs::read_to_string(ctx.home.join("audit/need-audit.report")).unwrap(),
        raw
    );
    assert!(host
        .logs()
        .iter()
        .any(|line| line.contains("audit report for need-audit: 2 fix units")));
}

#[test]
fn start_audit_dispatches_to_the_auditor_role() {
    let (_dir, ctx, host) = setup(Some(auditor_routing()));
    ready(&host);
    let mut st = State::default();
    let new = vec![String::from("gate::a_fails")];
    start_audit(&ctx, &mut st, TIP, "test result: FAILED", &new, &[], None);

    let dispatch = dispatch_command(&host);
    assert!(dispatch.contains("--pool apool"), "{dispatch}");
    assert!(dispatch.contains(&format!("--from {TIP}")), "{dispatch}");
    assert!(!dispatch.contains("--model"), "{dispatch}");
    assert!(!dispatch.contains("--read-only"), "{dispatch}");

    let run = current(&st).expect("no audit run stored");
    assert_eq!(run.task.as_deref(), Some("1234abcd"));
    assert_eq!(run.need, fix_need(&ctx, TIP));
    assert_eq!(run.pool.as_deref(), Some("apool"));
    let logs = host.logs();
    assert!(
        logs.iter().any(|line| line.contains("audit started for")),
        "{logs:?}"
    );
}

#[test]
fn start_audit_falls_back_to_the_gate_style_worker() {
    let (_dir, ctx, host) = setup(None);
    ready(&host);
    let mut st = State::default();
    start_audit(
        &ctx,
        &mut st,
        TIP,
        "tail",
        &[String::from("gate::a_fails")],
        &[],
        None,
    );

    let dispatch = dispatch_command(&host);
    assert!(dispatch.contains("--read-only"), "{dispatch}");
    assert!(dispatch.contains("--model gatemodel"), "{dispatch}");
    assert!(dispatch.contains("--pool apool"), "{dispatch}");
    assert_eq!(
        current(&st).and_then(|run| run.task).as_deref(),
        Some("1234abcd")
    );
}

#[test]
fn start_audit_skips_when_fix_units_exist() {
    let (_dir, ctx, host) = setup(None);
    ready(&host);
    let mut st = State::default();
    let unit = Unit {
        need: fix_need(&ctx, TIP),
        ..Unit::default()
    };
    st.units.insert(String::from("pumpfix-g01234567-1"), unit);
    start_audit(
        &ctx,
        &mut st,
        TIP,
        "tail",
        &[String::from("gate::a_fails")],
        &[],
        None,
    );

    let commands = host.commands();
    assert!(commands.is_empty(), "{commands:?}");
    assert!(current(&st).is_none());
}

#[test]
fn start_audit_owns_new_names_and_residuals() {
    let (_dir, ctx, host) = setup(None);
    ready(&host);
    let mut st = State::default();
    let new = vec![String::from("gate::a_fails")];
    let residual = vec![(
        String::from("crate::old"),
        String::from("pumpfix-gdeadbee0"),
    )];
    start_audit(&ctx, &mut st, TIP, "tail", &new, &residual, None);

    let owner = fix_need(&ctx, TIP);
    assert_eq!(st.red.get("gate::a_fails"), Some(&owner));
    assert_eq!(st.red.get("crate::old"), Some(&owner));
}
