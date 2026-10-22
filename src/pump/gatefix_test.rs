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
fn fix_ids_cover_pump_created_needs_and_their_units_only() {
    let mut st = red_state();
    st.needs.insert(
        String::from("4b2d1c9e"),
        crate::pump::state::NeedRecord {
            reason: crate::pump::audit::REPORT_REASON.to_owned(),
            ..crate::pump::state::NeedRecord::default()
        },
    );
    let mut target = crate::pump::state::TargetState::default();
    target
        .red
        .insert(String::from("gate::y"), String::from("3c89e015"));
    st.target_states.insert(String::from("gradus"), target);
    for (uid, need) in [
        ("fix-1", NEED),
        ("report-1", "4b2d1c9e"),
        ("gradus-1", "3c89e015"),
        ("plain-1", "plain"),
    ] {
        st.units.insert(
            String::from(uid),
            Unit {
                need: String::from(need),
                ..Unit::default()
            },
        );
    }

    let ids = crate::pump::gatefix::fix_ids(&st);

    for expected in [
        NEED, "4b2d1c9e", "3c89e015", "fix-1", "report-1", "gradus-1",
    ] {
        assert!(ids.contains(expected), "{expected} missing from {ids:?}");
    }
    assert!(!ids.contains("plain-1") && !ids.contains("plain"));
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
