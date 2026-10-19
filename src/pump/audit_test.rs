//! Tests for [`crate::pump::audit`]: audit state round-trips through `extra`
//! and the auditor brief carries failures, rules, and any prior report.

use std::collections::BTreeMap;

use tempfile::tempdir;

use crate::pump::audit::{AuditRun, audit_brief, current, start_audit, store};
use crate::pump::config::{GateConfig, PumpConfig};
use crate::pump::gatefix::fix_need;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{State, Unit};
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
    host.reply("pool status --porcelain", 0, "slot apool bridge slot-1 free");
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
    start_audit(&ctx, &mut st, TIP, "tail", &[String::from("gate::a_fails")], &[], None);

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
