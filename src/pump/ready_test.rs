//! Tests for [`super::ready`].

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::pump::config::{DispatchConfig, Lane, PumpConfig};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::ready::{is_ready, pick_model, satisfied};
use crate::pump::state::{Attempt, State, Status, Unit};

/// A `Ctx` over a fresh temp home with the given pump config.
fn setup_cfg(cfg: PumpConfig) -> (TempDir, Ctx, Arc<FakeHost>, State) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg);
    (dir, ctx, host, State::default())
}

/// The default test setup: default pump config, empty state.
fn setup() -> (TempDir, Ctx, Arc<FakeHost>, State) {
    setup_cfg(PumpConfig::default())
}

/// A `(handle, unit)` pair with the given status and write scope.
fn mk(id: &str, status: Status, scope: &[&str]) -> (String, Unit) {
    (
        id.to_string(),
        Unit {
            need: "n".to_string(),
            title: id.to_string(),
            write_scope: scope.iter().map(|s| (*s).to_string()).collect(),
            status,
            ..Unit::default()
        },
    )
}

/// A config with the given dispatch lanes and last-resort model.
fn lane_cfg(lanes: Vec<(&str, u32)>, last_resort: &str) -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.dispatch = DispatchConfig {
        lanes: lanes
            .into_iter()
            .map(|(model, cap)| Lane {
                model: model.to_string(),
                cap,
            })
            .collect(),
        last_resort: last_resort.to_string(),
        ..DispatchConfig::default()
    };
    cfg
}

#[test]
fn a_pending_unit_without_dependencies_or_overlap_is_ready() {
    let (_dir, ctx, _host, mut st) = setup();
    let (id, u) = mk("a-1", Status::Pending, &["src/a"]);
    st.units.insert(id, u);
    assert!(is_ready(&ctx, &st, "a-1"));
}

#[test]
fn an_unmerged_dependency_blocks_and_a_merged_one_does_not() {
    let (_dir, ctx, _host, mut st) = setup();
    let (dep_id, dep_u) = mk("dep", Status::Inflight, &["src/dep"]);
    st.units.insert(dep_id, dep_u);
    let (uid, mut u) = mk("u", Status::Pending, &["src/u"]);
    u.after = vec!["dep".to_string()];
    st.units.insert(uid, u);
    assert!(!is_ready(&ctx, &st, "u"));
    st.units.get_mut("dep").unwrap().status = Status::Merged;
    assert!(is_ready(&ctx, &st, "u"));
}

#[test]
fn a_no_defect_dependency_counts_as_satisfied() {
    let (_dir, ctx, _host, mut st) = setup();
    let (dep_id, dep_u) = mk("dep", Status::NoDefect, &["src/dep"]);
    st.units.insert(dep_id, dep_u);
    let (uid, mut u) = mk("u", Status::Pending, &["src/u"]);
    u.after = vec!["dep".to_string()];
    st.units.insert(uid, u);
    assert!(satisfied(&st, "dep", 0));
    assert!(is_ready(&ctx, &st, "u"));
}

#[test]
fn a_replaced_dependency_is_satisfied_only_when_every_replacement_is() {
    let (_dir, ctx, _host, mut st) = setup();
    let (dep_id, mut dep) = mk("dep", Status::Replaced, &["src/dep"]);
    dep.replaced_by = vec!["r1".to_string(), "r2".to_string()];
    st.units.insert(dep_id, dep);
    let (r1_id, r1_u) = mk("r1", Status::Merged, &["src/dep"]);
    let (r2_id, r2_u) = mk("r2", Status::Pending, &["src/dep"]);
    st.units.insert(r1_id, r1_u);
    st.units.insert(r2_id, r2_u);
    let (uid, mut u) = mk("u", Status::Pending, &["src/u"]);
    u.after = vec!["dep".to_string()];
    st.units.insert(uid, u);
    assert!(!satisfied(&st, "dep", 0));
    assert!(!is_ready(&ctx, &st, "u"));
    st.units.get_mut("r2").unwrap().status = Status::Merged;
    assert!(satisfied(&st, "dep", 0));
    assert!(is_ready(&ctx, &st, "u"));
}

#[test]
fn an_inflight_unit_with_an_overlapping_scope_blocks() {
    let (_dir, ctx, _host, mut st) = setup();
    let (bid, busy_u) = mk("busy", Status::Inflight, &["src/ovl"]);
    let (uid, uu) = mk("u", Status::Pending, &["src/ovl/x.rs"]);
    let (vid, vu) = mk("v", Status::Pending, &["src/elsewhere"]);
    st.units.insert(bid, busy_u);
    st.units.insert(uid, uu);
    st.units.insert(vid, vu);
    assert!(!is_ready(&ctx, &st, "u"));
    assert!(is_ready(&ctx, &st, "v"));
}

#[test]
fn a_delivered_or_merging_overlap_blocks_a_normal_unit_but_not_a_fix_unit() {
    let (_dir, ctx, _host, mut st) = setup();
    let (did, du) = mk("d", Status::Delivered, &["src/ovl"]);
    let (mid, mu) = mk("m", Status::Merging, &["src/ovl/deep.rs"]);
    let (uid, uu) = mk("u", Status::Pending, &["src/ovl/x.rs"]);
    let (fid, fu) = mk("pumpfix-u", Status::Pending, &["src/ovl/x.rs"]);
    st.units.insert(did, du);
    st.units.insert(mid, mu);
    st.units.insert(uid, uu);
    st.units.insert(fid, fu);
    assert!(!is_ready(&ctx, &st, "u"));
    // Fix units ignore delivered and merging overlaps; only inflight counts.
    assert!(is_ready(&ctx, &st, "pumpfix-u"));
}

#[test]
fn a_fix_unit_is_still_blocked_by_an_inflight_fix_unit_that_overlaps() {
    let (_dir, ctx, _host, mut st) = setup();
    let (aid, au) = mk("pumpfix-a", Status::Inflight, &["src/ovl"]);
    let (bid, bu) = mk("pumpfix-b", Status::Pending, &["src/ovl/x.rs"]);
    let (cid, cu) = mk("pumpfix-c", Status::Pending, &["src/other"]);
    st.units.insert(aid, au);
    st.units.insert(bid, bu);
    st.units.insert(cid, cu);
    assert!(!is_ready(&ctx, &st, "pumpfix-b"));
    assert!(is_ready(&ctx, &st, "pumpfix-c"));
}

#[test]
fn a_unit_that_is_not_pending_is_not_ready() {
    let (_dir, ctx, _host, mut st) = setup();
    for status in [
        Status::Inflight,
        Status::Delivered,
        Status::Merging,
        Status::Merged,
        Status::NoDefect,
        Status::Stuck,
        Status::Splitting,
        Status::Replaced,
    ] {
        let (uid, uu) = mk("u", status, &["src/a"]);
        st.units.insert(uid, uu);
        assert!(!is_ready(&ctx, &st, "u"));
        st.units.remove("u");
    }
}

#[test]
fn pick_model_alternates_lanes_by_attempt_parity() {
    let mut cfg = lane_cfg(vec![("m-a", 1), ("m-b", 1)], "");
    // Bump max_attempts so attempts 0-2 all stay in the lane walk.
    cfg.dispatch.max_attempts = 5;
    let (_dir, ctx, _host, st) = setup_cfg(cfg);
    let even = Unit {
        attempts: 0,
        ..Unit::default()
    };
    assert_eq!(pick_model(&ctx, &st, &even).as_deref(), Some("m-a"));
    let mut odd = Unit::default();
    odd.attempts = 1;
    assert_eq!(pick_model(&ctx, &st, &odd).as_deref(), Some("m-b"));
    let mut even_again = Unit::default();
    even_again.attempts = 2;
    assert_eq!(pick_model(&ctx, &st, &even_again).as_deref(), Some("m-a"));
}

#[test]
fn pick_model_avoids_the_last_model_and_respects_caps() {
    let (_dir, ctx, _host, mut st) = setup_cfg(lane_cfg(vec![("m-a", 1), ("m-b", 1)], ""));
    // The last attempt's model is skipped while another lane is free.
    let u = Unit {
        history: vec![Attempt {
            model: Some("m-a".to_string()),
            ..Attempt::default()
        }],
        ..Unit::default()
    };
    assert_eq!(pick_model(&ctx, &st, &u).as_deref(), Some("m-b"));
    // A lane at its cap is skipped even though it is not the last model.
    let (bid, mut busy) = mk("busy", Status::Inflight, &["src/a"]);
    busy.model = Some("m-a".to_string());
    st.units.insert(bid, busy);

    let fresh = Unit::default();
    assert_eq!(pick_model(&ctx, &st, &fresh).as_deref(), Some("m-b"));
    // Preference pass finds nothing (m-a full, m-b is the last model), so
    // the fallback pass returns the under-cap m-b anyway.
    let retried = Unit {
        history: vec![Attempt {
            model: Some("m-b".to_string()),
            ..Attempt::default()
        }],
        ..Unit::default()
    };
    assert_eq!(pick_model(&ctx, &st, &retried).as_deref(), Some("m-b"));
}

#[test]
fn pick_model_returns_the_last_resort_on_the_final_attempt() {
    let (_dir, ctx, _host, st) = setup_cfg(lane_cfg(vec![("m-a", 1)], "m-last"));
    let final_attempt = Unit {
        attempts: 2, // max_attempts defaults to 3, so this is the last try
        ..Unit::default()
    };
    assert_eq!(
        pick_model(&ctx, &st, &final_attempt).as_deref(),
        Some("m-last")
    );
}

#[test]
fn pick_model_returns_none_when_every_lane_is_full() {
    let (_dir, ctx, _host, mut st) = setup_cfg(lane_cfg(vec![("m-a", 1), ("m-b", 1)], ""));
    let (b1, mut busy1) = mk("b1", Status::Inflight, &["src/1"]);
    busy1.model = Some("m-a".to_string());
    st.units.insert(b1, busy1);
    let (b2, mut busy2) = mk("b2", Status::Inflight, &["src/2"]);
    busy2.model = Some("m-b".to_string());
    st.units.insert(b2, busy2);
    let u = Unit::default();
    assert_eq!(pick_model(&ctx, &st, &u), None);
}
