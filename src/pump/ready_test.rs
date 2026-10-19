//! Tests for [`super::ready`].

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::pump::config::{DispatchConfig, Generated, Lane, MergeConfig, PumpConfig};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::ready::{dependency_holds, is_ready, pick_model, ranked, satisfied};
use crate::pump::state::{Attempt, NeedRecord, State, Status, Unit};

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
    PumpConfig {
        dispatch: DispatchConfig {
            lanes: lanes
                .into_iter()
                .map(|(model, cap)| Lane {
                    model: model.to_string(),
                    cap,
                })
                .collect(),
            last_resort: last_resort.to_string(),
            ..DispatchConfig::default()
        },
        ..PumpConfig::default()
    }
}

#[test]
fn a_pending_unit_without_dependencies_or_overlap_is_ready() {
    let (_dir, ctx, _host, mut st) = setup();
    let (id, u) = mk("a-1", Status::Pending, &["src/a"]);
    st.units.insert(id, u);
    assert!(is_ready(&ctx, &st, "a-1", &BTreeMap::new()));
}

#[test]
fn an_unmerged_dependency_blocks_and_a_merged_one_does_not() {
    let (_dir, ctx, _host, mut st) = setup();
    let (dep_id, dep_u) = mk("dep", Status::Inflight, &["src/dep"]);
    st.units.insert(dep_id, dep_u);
    let (uid, mut u) = mk("u", Status::Pending, &["src/u"]);
    u.after = vec!["dep".to_string()];
    st.units.insert(uid, u);
    assert!(!is_ready(&ctx, &st, "u", &BTreeMap::new()));
    st.units.get_mut("dep").unwrap().status = Status::Merged;
    assert!(is_ready(&ctx, &st, "u", &BTreeMap::new()));
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
    assert!(is_ready(&ctx, &st, "u", &BTreeMap::new()));
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
    assert!(!is_ready(&ctx, &st, "u", &BTreeMap::new()));
    st.units.get_mut("r2").unwrap().status = Status::Merged;
    assert!(satisfied(&st, "dep", 0));
    assert!(is_ready(&ctx, &st, "u", &BTreeMap::new()));
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
    assert!(!is_ready(&ctx, &st, "u", &BTreeMap::new()));
    assert!(is_ready(&ctx, &st, "v", &BTreeMap::new()));
}

/// A config whose merge lane regenerates `scripta/ledger.toml`.
fn generated_cfg() -> PumpConfig {
    PumpConfig {
        merge: MergeConfig {
            generated: vec![Generated {
                paths: vec![String::from("scripta/ledger.toml")],
                regenerate: String::from("./regen-ledger.sh"),
                drift: vec![],
            }],
            ..MergeConfig::default()
        },
        ..PumpConfig::default()
    }
}

#[test]
fn a_generated_path_overlap_does_not_block_dispatch() {
    let (_dir, ctx, _host, mut st) = setup_cfg(generated_cfg());
    let (bid, busy_u) = mk(
        "busy",
        Status::Inflight,
        &["src/a.rs", "scripta/ledger.toml"],
    );
    let (uid, uu) = mk("u", Status::Pending, &["src/b.rs", "scripta/ledger.toml"]);
    st.units.insert(bid, busy_u);
    st.units.insert(uid, uu);
    assert!(is_ready(&ctx, &st, "u", &BTreeMap::new()));
    // Control: an overlap on a hand-written path still blocks.
    let (vid, vu) = mk("v", Status::Pending, &["src/a.rs", "src/v.rs"]);
    st.units.insert(vid, vu);
    assert!(!is_ready(&ctx, &st, "v", &BTreeMap::new()));
}

#[test]
fn delivered_and_merging_overlaps_do_not_block_new_units() {
    let (_dir, ctx, _host, mut st) = setup();
    let (did, du) = mk("d", Status::Delivered, &["src/ovl"]);
    let (mid, mu) = mk("m", Status::Merging, &["src/ovl/deep.rs"]);
    let (uid, uu) = mk("u", Status::Pending, &["src/ovl/x.rs"]);
    let (fid, fu) = mk("pumpfix-u", Status::Pending, &["src/ovl/x.rs"]);
    st.units.insert(did, du);
    st.units.insert(mid, mu);
    st.units.insert(uid, uu);
    st.units.insert(fid, fu);
    assert!(is_ready(&ctx, &st, "u", &BTreeMap::new()));
    assert!(is_ready(&ctx, &st, "pumpfix-u", &BTreeMap::new()));
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
    assert!(!is_ready(&ctx, &st, "pumpfix-b", &BTreeMap::new()));
    assert!(is_ready(&ctx, &st, "pumpfix-c", &BTreeMap::new()));
}

#[test]
fn a_quarantine_repair_is_ready_despite_an_inflight_ordinary_unit_sharing_its_scope() {
    let (_dir, ctx, _host, mut st) = setup();
    // An ordinary unit inflight on `src/runner.rs`.
    let (bid, busy_u) = mk("corpus-9", Status::Inflight, &["src/runner.rs"]);
    // A quarantine repair unit whose write scope covers `src/runner.rs` too.
    let (rid, mut ru) = mk(
        "pumpfix-q1-aaaaaa",
        Status::Pending,
        &["src/runner.rs", "outcomes.json"],
    );
    ru.repairs = Some(String::from("q1-aaaaaa"));
    ru.base = Some(String::from("origin/pump/quarantine/q1-aaaaaa"));
    st.units.insert(bid, busy_u);
    st.units.insert(rid, ru);

    // The repair is the path back to green: it ignores the ordinary inflight
    // overlap and outranks the ordinary unit for dispatch.
    assert!(is_ready(&ctx, &st, "pumpfix-q1-aaaaaa", &BTreeMap::new()));
    assert_eq!(
        ranked(
            &ctx,
            &st,
            vec![String::from("corpus-9"), String::from("pumpfix-q1-aaaaaa")]
        ),
        ["pumpfix-q1-aaaaaa", "corpus-9"]
    );

    // The serialization still runs the other way: an ordinary unit waits
    // behind an ordinary inflight overlap ...
    let (oid, ou) = mk("corpus-10", Status::Pending, &["src/runner.rs"]);
    st.units.insert(oid, ou);
    assert!(!is_ready(&ctx, &st, "corpus-10", &BTreeMap::new()));
    // ... and an ordinary unit waits for a busy repair too.
    st.units
        .get_mut("pumpfix-q1-aaaaaa")
        .unwrap()
        .status = Status::Inflight;
    st.units.remove("corpus-9");
    assert!(!is_ready(&ctx, &st, "corpus-10", &BTreeMap::new()));
}

#[test]
fn equal_relative_scopes_in_different_targets_do_not_serialize() {
    let (_dir, ctx, _host, mut st) = setup();
    let (aid, mut au) = mk("a", Status::Inflight, &["src/lib.rs"]);
    au.target = String::from("radix");
    let (bid, mut bu) = mk("b", Status::Pending, &["src/lib.rs"]);
    bu.target = String::from("tela");
    st.units.insert(aid, au);
    st.units.insert(bid, bu);
    assert!(is_ready(&ctx, &st, "b", &BTreeMap::new()));
}

#[test]
fn a_unit_that_is_not_pending_is_not_ready() {
    let (_dir, ctx, _host, mut st) = setup();
    for status in [
        Status::Held,
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
        assert!(!is_ready(&ctx, &st, "u", &BTreeMap::new()));
        st.units.remove("u");
    }
}

#[test]
fn a_unit_inside_its_backoff_window_is_not_ready() {
    let (_dir, ctx, _host, mut st) = setup();
    let now = ctx.now();
    let (uid, mut u) = mk("u", Status::Pending, &["src/a"]);
    u.retry_not_before = Some(now + 300);
    st.units.insert(uid, u);
    assert!(!is_ready(&ctx, &st, "u", &BTreeMap::new()));
    st.units.get_mut("u").unwrap().retry_not_before = Some(now - 1);
    assert!(is_ready(&ctx, &st, "u", &BTreeMap::new()));
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
    let odd = Unit {
        attempts: 1,
        ..Unit::default()
    };
    assert_eq!(pick_model(&ctx, &st, &odd).as_deref(), Some("m-b"));
    let even_again = Unit {
        attempts: 2,
        ..Unit::default()
    };
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

#[test]
fn pick_model_uses_the_normal_lanes_on_the_final_attempt_without_a_last_resort() {
    let (_dir, ctx, _host, st) = setup_cfg(lane_cfg(vec![("m-a", 1)], ""));
    let final_attempt = Unit {
        attempts: 2,
        ..Unit::default()
    };
    assert_eq!(
        pick_model(&ctx, &st, &final_attempt).as_deref(),
        Some("m-a")
    );
}

/// Write the configured reserved-paths file.
fn write_reserved(ctx: &Ctx, text: &str) {
    let path = ctx.cfg.reserved_path(&ctx.project);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn a_reserved_glob_blocks_an_overlapping_unit_but_not_a_disjoint_one() {
    let (_dir, ctx, _host, mut st) = setup();
    write_reserved(
        &ctx,
        "# held for another session\n\n  crates/held/*.rs  \n#src/a\n",
    );
    let (a, ua) = mk("a-1", Status::Pending, &["crates/held/lib.rs"]);
    let (b, ub) = mk("b-1", Status::Pending, &["crates/other/lib.rs"]);
    let (c, uc) = mk("c-1", Status::Pending, &["src/a"]);
    st.units.insert(a.clone(), ua);
    st.units.insert(b.clone(), ub);
    st.units.insert(c.clone(), uc);
    assert!(!is_ready(&ctx, &st, &a, &BTreeMap::new()));
    assert!(is_ready(&ctx, &st, &b, &BTreeMap::new()));
    // A commented-out line holds nothing.
    assert!(is_ready(&ctx, &st, &c, &BTreeMap::new()));
}

#[test]
fn the_configured_reserved_file_is_read_not_a_fixed_name() {
    let (_dir, mut ctx, _host, mut st) = setup();
    let mut cfg = (*ctx.cfg).clone();
    cfg.reserved_file = String::from("held/paths.list");
    ctx.cfg = std::sync::Arc::new(cfg);
    let (id, u) = mk("a-1", Status::Pending, &["src/a/x.rs"]);
    st.units.insert(id.clone(), u);
    // A file under the old fixed name holds nothing any more.
    std::fs::write(ctx.home.join("reserved.txt"), "src/a\n").unwrap();
    assert!(is_ready(&ctx, &st, &id, &BTreeMap::new()));
    write_reserved(&ctx, "src/a\n");
    assert!(
        ctx.cfg
            .reserved_path(&ctx.project)
            .ends_with("held/paths.list")
    );
    assert!(!is_ready(&ctx, &st, &id, &BTreeMap::new()));
}

#[test]
fn reserved_txt_is_re_read_each_call_and_may_be_absent() {
    let (_dir, ctx, _host, mut st) = setup();
    let (id, u) = mk("a-1", Status::Pending, &["src/a/x.rs"]);
    st.units.insert(id.clone(), u);
    assert!(is_ready(&ctx, &st, &id, &BTreeMap::new())); // no file
    write_reserved(&ctx, "src/a\n");
    assert!(!is_ready(&ctx, &st, &id, &BTreeMap::new()));
    write_reserved(&ctx, "src/b\n");
    assert!(is_ready(&ctx, &st, &id, &BTreeMap::new()));
}

#[test]
fn a_need_dependency_hold_blocks_its_units_until_the_dependency_lands() {
    let (_dir, ctx, host, mut st) = setup();
    host.reply(
        "graph blocked",
        0,
        r#"[{"source_id":"n","kind":"need","label":"N","blocked_by":["dep"]}]"#,
    );
    let (id, u) = mk("n-1", Status::Pending, &["src/u"]);
    st.units.insert(id, u);

    let holds = dependency_holds(&ctx, &st);
    assert_eq!(holds["n"], vec![String::from("dep")]);
    assert!(!is_ready(&ctx, &st, "n-1", &holds));

    // The pump already merged every unit of the dependency, so it counts as
    // landed even though its Vivi node is still open.
    st.needs.insert(
        String::from("dep"),
        NeedRecord {
            units: vec![String::from("dep-1")],
            ..NeedRecord::default()
        },
    );
    st.units.insert(
        String::from("dep-1"),
        Unit {
            status: Status::Merged,
            ..Unit::default()
        },
    );
    let holds = dependency_holds(&ctx, &st);
    assert!(holds.is_empty());
    assert!(is_ready(&ctx, &st, "n-1", &holds));
}

#[test]
fn an_unreadable_dependency_gate_holds_nothing_and_logs_it() {
    let (_dir, ctx, host, mut st) = setup();
    host.reply("graph blocked", 1, "error: no such graph");
    let (id, u) = mk("n-1", Status::Pending, &["src/u"]);
    st.units.insert(id, u);

    assert!(dependency_holds(&ctx, &st).is_empty());
    assert!(is_ready(&ctx, &st, "n-1", &BTreeMap::new()));
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("dependency gate unreadable"))
    );
}

#[test]
fn delivered_overlaps_do_not_block_priority_or_normal_units() {
    let (_dir, ctx, _host, mut st) = setup();
    crate::pump::priority::set(&ctx.home, "hot", 1).unwrap();
    let (did, du) = mk("d", Status::Delivered, &["src/ovl"]);
    let (uid, mut uu) = mk("u", Status::Pending, &["src/ovl/x.rs"]);
    let (vid, vu) = mk("v", Status::Pending, &["src/ovl/y.rs"]);
    uu.need = String::from("hot");
    st.units.insert(did, du);
    st.units.insert(uid, uu);
    st.units.insert(vid, vu);
    assert!(is_ready(&ctx, &st, "u", &BTreeMap::new()));
    assert!(is_ready(&ctx, &st, "v", &BTreeMap::new()));
}

#[test]
fn ranked_puts_fix_units_first_then_higher_levels_then_the_rest_stably() {
    let (_dir, ctx, _host, mut st) = setup();
    for (uid, need) in [
        ("a", "plain"),
        ("b", "low"),
        ("c", "high"),
        ("pumpfix-1", "plain"),
        ("d", "plain"),
        ("e", "high"),
        ("f", "low"),
    ] {
        let (id, mut unit) = mk(uid, Status::Pending, &[]);
        unit.need = need.to_string();
        st.units.insert(id, unit);
    }
    crate::pump::priority::set(&ctx.home, "low", 1).unwrap();
    crate::pump::priority::set(&ctx.home, "high", 5).unwrap();
    let uids: Vec<String> = ["a", "b", "c", "pumpfix-1", "d", "e", "f"]
        .iter()
        .map(|uid| (*uid).to_string())
        .collect();
    assert_eq!(
        ranked(&ctx, &st, uids),
        ["pumpfix-1", "c", "e", "b", "f", "a", "d"]
    );
}

#[test]
fn ranked_keeps_plan_order_without_priorities() {
    let (_dir, ctx, _host, mut st) = setup();
    let (id, unit) = mk("x", Status::Pending, &[]);
    st.units.insert(id, unit);
    let uids = vec!["z".to_string(), "x".to_string(), "y".to_string()];
    assert_eq!(ranked(&ctx, &st, uids), ["z", "x", "y"]);
}
