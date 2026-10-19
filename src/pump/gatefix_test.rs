//! Tests for residual-name ownership in the auditor flow.

use std::fs;

use tempfile::tempdir;

use crate::pump::audit::{AuditRun, store};
use crate::pump::config::PumpConfig;
use crate::pump::gatefix::orphaned;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{State, Status, Unit};

/// The fix need that owns the red name.
const NEED: &str = "pumpfix-g01234567";

/// A context and fake host for one red-owner case.
fn setup() -> (tempfile::TempDir, Ctx, std::sync::Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    (dir, ctx, host)
}

/// A state with a red name owned by [`NEED`].
fn red_state() -> State {
    let mut st = State::default();
    st.red.insert(String::from("crate::x"), String::from(NEED));
    st
}

#[test]
fn orphaned_requires_no_units_no_plan_and_no_live_audit() {
    let (dir, ctx, _host) = setup();
    let mut st = red_state();
    assert!(orphaned(&ctx, &st, NEED));

    st.units.insert(
        String::from("unit-1"),
        Unit {
            need: String::from(NEED),
            status: Status::Pending,
            ..Unit::default()
        },
    );
    assert!(!orphaned(&ctx, &st, NEED));
    st.units.clear();

    let plans = ctx.home.join("plans");
    fs::create_dir_all(&plans).unwrap();
    fs::write(plans.join(format!("{NEED}.json")), "{}").unwrap();
    assert!(!orphaned(&ctx, &st, NEED));
    fs::remove_file(plans.join(format!("{NEED}.json"))).unwrap();

    store(
        &mut st,
        Some(AuditRun {
            need: String::from(NEED),
            task: Some(String::from("audit-task")),
            ..AuditRun::default()
        }),
    );
    assert!(!orphaned(&ctx, &st, NEED));
    store(&mut st, None);
    assert!(orphaned(&ctx, &st, NEED));
    drop(dir);
}
