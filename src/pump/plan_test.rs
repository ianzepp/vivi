//! Tests for [`super::plan`].

use std::fs;
use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use crate::pump::config::{PumpConfig, PumpTarget};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::plan::{ingest, overlaps};
use crate::pump::state::{State, Status, Unit};

/// A `Ctx` over a fresh temp home with an empty `plans/` directory.
fn setup() -> (TempDir, Ctx, Arc<FakeHost>, State) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    (dir, ctx, host, State::default())
}

/// Write a plan file into the home's `plans/` directory.
fn write_plan(ctx: &Ctx, name: &str, plan: &Value) {
    fs::write(ctx.home.join("plans").join(name), plan.to_string()).unwrap();
}

#[test]
fn overlaps_is_true_for_equal_nested_and_empty_roots() {
    assert!(overlaps(&["src/a".to_string()], &["src/a".to_string()]));
    assert!(overlaps(
        &["src/a/x.rs".to_string()],
        &["src/a".to_string()]
    ));
    assert!(overlaps(&["src".to_string()], &["src/b/y.rs".to_string()]));
    // A glob-only scope has an empty root, which overlaps everything.
    assert!(overlaps(
        &["*.rs".to_string()],
        &["tgt/anything".to_string()]
    ));
}

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn a_glob_blocks_only_paths_it_can_match() {
    let held = v(&[
        "Cargo.toml",
        "crates/*/Cargo.toml",
        "crates/radix-mir-llvm/**",
    ]);
    // The reservation this fix is for: a unit elsewhere under crates/ is free.
    assert!(!overlaps(
        &v(&["crates/radix-semantic/src/check.rs"]),
        &held
    ));
    assert!(!overlaps(
        &v(&["crates/faber/src/cli.rs", "crates/faber/src/cli_test.rs"]),
        &held
    ));
    // A root Cargo.toml entry does not hold a crate's Cargo.toml by itself...
    assert!(!overlaps(&v(&["lib/Cargo.toml"]), &v(&["Cargo.toml"])));
    // ...but the crate glob does, and so does the whole crate directory.
    assert!(overlaps(&v(&["crates/faber/Cargo.toml"]), &held));
    assert!(overlaps(&v(&["crates/radix-semantic"]), &held));
    assert!(overlaps(&v(&["crates/radix-mir-llvm/src/emit.rs"]), &held));
    assert!(overlaps(&v(&["Cargo.toml"]), &held));
}

#[test]
fn double_star_and_single_segment_globs() {
    assert!(overlaps(&v(&["src/**/x.rs"]), &v(&["src/a/b/x.rs"])));
    assert!(overlaps(&v(&["src/**"]), &v(&["src"])));
    assert!(!overlaps(&v(&["src/**/x.rs"]), &v(&["lib/a/x.rs"])));
    assert!(overlaps(&v(&["src/*_test.rs"]), &v(&["src/plan_test.rs"])));
    assert!(!overlaps(&v(&["src/*_test.rs"]), &v(&["src/plan.rs"])));
    assert!(overlaps(&v(&["src/?.rs"]), &v(&["src/a.rs"])));
    assert!(overlaps(&v(&["src/[ab].rs"]), &v(&["src/a.rs"])));
    // Two globs at the same position are assumed to meet.
    assert!(overlaps(
        &v(&["crates/*/src"]),
        &v(&["crates/radix-*/src/lib.rs"])
    ));
}

#[test]
fn a_bare_glob_matches_at_any_depth_and_an_empty_scope_covers_everything() {
    assert!(overlaps(&v(&["*.rs"]), &v(&["deep/down/file.rs"])));
    // A scope also covers what is under it, so `deep/down/file.md` could hold a `.rs` file.
    assert!(overlaps(&v(&["*.rs"]), &v(&["deep/down/file.md"])));
    assert!(!overlaps(&v(&["src/*.rs"]), &v(&["src/file.md"])));
    assert!(overlaps(&v(&[""]), &v(&["anything/at/all"])));
    assert!(overlaps(&v(&["./src/"]), &v(&["src/x.rs"])));
}

#[test]
fn overlaps_is_false_for_sibling_paths() {
    assert!(!overlaps(&["src/a".to_string()], &["src/b".to_string()]));
    assert!(!overlaps(&["src/ab".to_string()], &["src/a".to_string()]));
    assert!(!overlaps(
        &["src/a/x.rs".to_string()],
        &["src/b/y.rs".to_string()]
    ));
    assert!(!overlaps(
        &["crates/one/src".to_string()],
        &["crates/two/src".to_string()]
    ));
}

#[test]
fn ingest_creates_pending_units_and_a_need_record() {
    let (_dir, ctx, host, mut st) = setup();
    let long_title = "x".repeat(200);
    write_plan(
        &ctx,
        "56f1d749.json",
        &json!({
            "need": "56f1d749",
            "verdict": "units",
            "reason": "pump the bridge",
            "units": [
                {"id": "56f1d749-1", "title": long_title, "brief": "do the thing",
                 "write_scope": ["src/a.rs"], "after": [], "proof": ["cargo test"],
                 "tools": "plain"},
                {"id": "56f1d749-2", "brief": "do the other thing",
                 "write_scope": ["src/b.rs"]}
            ]
        }),
    );
    // A file that is not valid JSON is skipped silently (and never marked
    // seen, so it is re-read on every tick).
    fs::write(ctx.home.join("plans").join("zz-broken.json"), "not json").unwrap();
    ingest(&ctx, &mut st);
    assert_eq!(st.units.len(), 2);
    let u = &st.units["56f1d749-1"];
    assert_eq!(u.need, "56f1d749");
    assert_eq!(u.status, Status::Pending);
    assert_eq!(u.attempts, 0);
    assert_eq!(u.since, ctx.now());
    assert_eq!(u.title.len(), 150);
    assert_eq!(u.proof, vec!["cargo test".to_string()]);
    assert_eq!(u.tools, "plain");
    let u2 = &st.units["56f1d749-2"];
    assert_eq!(u2.title, "");
    assert_eq!(u2.tools, "", "no tools value means the default kind");
    assert_eq!(st.order, vec!["56f1d749-1", "56f1d749-2"]);
    let rec = &st.needs["56f1d749"];
    assert_eq!(rec.verdict, "units");
    assert_eq!(rec.reason, "pump the bridge");
    assert_eq!(rec.units, vec!["56f1d749-1", "56f1d749-2"]);
    assert_eq!(st.plans_seen, vec!["56f1d749.json".to_string()]);
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "plan 56f1d749 verdict=units units=2")
    );
}

#[test]
fn one_need_can_assign_units_to_two_workspace_targets() {
    let dir = tempdir().unwrap();
    let mut cfg = PumpConfig::default();
    cfg.targets = [
        (
            String::from("radix"),
            PumpTarget {
                path: String::from("radix"),
                branch: String::from("next"),
                ..PumpTarget::default()
            },
        ),
        (
            String::from("tela"),
            PumpTarget {
                path: String::from("tela"),
                branch: String::from("main"),
                ..PumpTarget::default()
            },
        ),
    ]
    .into();
    let (ctx, host) = test_ctx(dir.path(), cfg);
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    let mut st = State::default();
    write_plan(
        &ctx,
        "need.json",
        &json!({
            "need": "need",
            "units": [
                {"id": "need-1", "brief": "radix work", "target": "radix", "write_scope": ["src/lib.rs"]},
                {"id": "need-2", "brief": "tela work", "target": "tela", "write_scope": ["src/lib.rs"]}
            ]
        }),
    );
    ingest(&ctx, &mut st);
    assert_eq!(st.units["need-1"].target, "radix");
    assert_eq!(st.units["need-2"].target, "tela");
    assert_eq!(st.needs["need"].units, vec!["need-1", "need-2"]);
    drop(host);
}

#[test]
fn explicit_target_map_holds_a_plan_unit_with_no_target() {
    let dir = tempdir().unwrap();
    let mut cfg = PumpConfig::default();
    cfg.targets = [(
        String::from("radix"),
        PumpTarget {
            path: String::from("radix"),
            branch: String::from("next"),
            ..PumpTarget::default()
        },
    )]
    .into();
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    let mut st = State::default();
    write_plan(
        &ctx,
        "need.json",
        &json!({
            "need": "need",
            "units": [{"id": "need-1", "brief": "work", "write_scope": ["src/lib.rs"]}]
        }),
    );
    ingest(&ctx, &mut st);
    assert!(st.units.is_empty());
    assert_eq!(st.needs["need"].verdict, "hold");
    assert!(
        st.needs["need"]
            .reason
            .contains("must name a configured repository target")
    );
}

#[test]
fn a_revision_cannot_move_an_attempted_unit_to_another_target() {
    let dir = tempdir().unwrap();
    let mut cfg = PumpConfig::default();
    cfg.targets = ["radix", "tela"]
        .into_iter()
        .map(|id| {
            (
                id.to_string(),
                PumpTarget {
                    path: id.to_string(),
                    branch: String::from("main"),
                    ..PumpTarget::default()
                },
            )
        })
        .collect();
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    let mut st = State::default();
    write_plan(
        &ctx,
        "need.json",
        &json!({
            "need": "need",
            "units": [{"id": "need-1", "brief": "work", "target": "radix", "write_scope": ["src/lib.rs"]}]
        }),
    );
    ingest(&ctx, &mut st);
    st.units.get_mut("need-1").unwrap().attempts = 1;
    write_plan(
        &ctx,
        "need.r2.json",
        &json!({
            "need": "need",
            "revision": 2,
            "units": [{"id": "need-1", "brief": "work", "target": "tela", "write_scope": ["src/lib.rs"]}]
        }),
    );
    ingest(&ctx, &mut st);
    assert_eq!(st.units["need-1"].target, "radix");
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exceptions.contains("refused to change repository target after an attempt"));
}

#[test]
fn a_cross_target_prerequisite_parks_the_need_before_dispatch() {
    let dir = tempdir().unwrap();
    let mut cfg = PumpConfig::default();
    cfg.targets = ["radix", "tela"]
        .into_iter()
        .map(|id| {
            (
                id.to_string(),
                PumpTarget {
                    path: id.to_string(),
                    branch: String::from("main"),
                    ..PumpTarget::default()
                },
            )
        })
        .collect();
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    let mut st = State::default();
    write_plan(
        &ctx,
        "need.json",
        &json!({
            "need": "need",
            "units": [
                {"id": "need-1", "brief": "radix change", "target": "radix", "write_scope": ["src/lib.rs"]},
                {"id": "need-2", "brief": "use the radix change", "target": "tela", "write_scope": ["src/lib.rs"], "after": ["need-1"]}
            ]
        }),
    );
    ingest(&ctx, &mut st);
    assert!(st.units.is_empty());
    assert_eq!(st.needs["need"].verdict, "hold");
    assert!(
        st.needs["need"]
            .reason
            .contains("unpublished branch handoff")
    );
}

#[test]
fn ingest_skips_a_plan_file_seen_before() {
    let (_dir, ctx, host, mut st) = setup();
    write_plan(
        &ctx,
        "56f1d749.json",
        &json!({
            "need": "56f1d749",
            "verdict": "units",
            "reason": "",
            "units": [{"id": "56f1d749-1", "brief": "b", "write_scope": ["src/a.rs"]}]
        }),
    );
    st.plans_seen.push("56f1d749.json".to_string());
    ingest(&ctx, &mut st);
    assert!(st.units.is_empty());
    assert!(st.order.is_empty());
    assert!(st.needs.is_empty());
    assert_eq!(st.plans_seen, vec!["56f1d749.json".to_string()]);
    assert!(!host.logs().iter().any(|l| l.starts_with("plan ")));
}

#[test]
fn ingest_rejects_a_unit_without_a_brief_and_raises_bad_plan() {
    let (_dir, ctx, host, mut st) = setup();
    write_plan(
        &ctx,
        "bad.json",
        &json!({
            "need": "56f1d749",
            "verdict": "units",
            "units": [{"id": "56f1d749-1", "write_scope": ["src/a.rs"], "proof": []}]
        }),
    );
    ingest(&ctx, &mut st);
    assert!(st.units.is_empty());
    assert!(st.order.is_empty());
    let rec = &st.needs["56f1d749"];
    assert_eq!(rec.verdict, "invalid");
    assert_eq!(rec.reason, "unit missing id, brief or write_scope");
    assert_eq!(st.plans_seen, vec!["bad.json".to_string()]);
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "EXCEPTION bad-plan bad.json")
    );
    let exc = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exc.contains("bad-plan bad.json unit missing id, brief or write_scope"));
}

#[test]
fn ingest_rejects_a_duplicate_unit_id() {
    let (_dir, ctx, host, mut st) = setup();
    st.units.insert("56f1d749-1".to_string(), Unit::default());
    write_plan(
        &ctx,
        "dup.json",
        &json!({
            "need": "56f1d749",
            "verdict": "units",
            "units": [
                {"id": "56f1d749-1", "brief": "b", "write_scope": ["src/a.rs"]},
                {"id": "56f1d749-2", "brief": "b", "write_scope": ["src/b.rs"]}
            ]
        }),
    );
    ingest(&ctx, &mut st);
    assert_eq!(st.units.len(), 1);
    assert!(st.order.is_empty());
    assert_eq!(st.needs["56f1d749"].verdict, "invalid");
    assert_eq!(st.needs["56f1d749"].reason, "duplicate unit id 56f1d749-1");
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "EXCEPTION bad-plan dup.json")
    );
}

#[test]
fn ingest_a_split_plan_replaces_the_unit_and_an_empty_split_raises_split_empty() {
    // A split with units replaces the old unit and extends the need record.
    let (_dir, ctx, host, mut st) = setup();
    st.units.insert(
        "u1".to_string(),
        Unit {
            need: "n1".to_string(),
            ..Unit::default()
        },
    );
    write_plan(
        &ctx,
        "u1.split.json",
        &json!({
            "need": "n1",
            "replaces": "u1",
            "verdict": "units",
            "reason": "too big for one budget",
            "units": [
                {"id": "u1.1", "brief": "half one", "write_scope": ["src/a.rs"]},
                {"id": "u1.2", "brief": "half two", "write_scope": ["src/b.rs"]}
            ]
        }),
    );
    ingest(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Replaced);
    assert_eq!(st.units["u1"].replaced_by, vec!["u1.1", "u1.2"]);
    assert_eq!(st.needs["n1"].verdict, "units");
    assert_eq!(st.needs["n1"].units, vec!["u1.1", "u1.2"]);
    assert!(host.logs().iter().any(|l| l == "split u1 into 2 units"));

    // An empty split files split-empty and replaces with no successors.
    let (_dir2, ctx2, host2, mut st2) = setup();
    st2.units.insert(
        "u2".to_string(),
        Unit {
            need: "n1".to_string(),
            ..Unit::default()
        },
    );
    write_plan(
        &ctx2,
        "u2.split.json",
        &json!({
            "need": "n1",
            "replaces": "u2",
            "reason": "nothing to do",
            "units": []
        }),
    );
    ingest(&ctx2, &mut st2);
    assert_eq!(st2.units["u2"].status, Status::Replaced);
    assert!(st2.units["u2"].replaced_by.is_empty());
    let exc = fs::read_to_string(ctx2.home.join("exceptions.txt")).unwrap();
    assert!(exc.contains("split-empty u2 nothing to do"));
    assert!(host2.logs().iter().any(|l| l == "split u2 into 0 units"));
}

/// One unit in a plan body.
fn plan_unit(id: &str, brief: &str) -> Value {
    json!({
        "id": id,
        "title": id,
        "brief": brief,
        "write_scope": ["src/a.rs"],
        "proof": [],
        "tools": "plain"
    })
}

/// A first plan for `n1` with one unit.
fn first_plan() -> Value {
    json!({
        "need": "n1",
        "revision": 1,
        "verdict": "units",
        "reason": "first",
        "units": [plan_unit("n1-1", "first pass")]
    })
}

#[test]
fn ingest_rejects_a_unit_with_an_invalid_proof_command() {
    let (_dir, ctx, host, mut st) = setup();
    write_plan(
        &ctx,
        "bad.json",
        &json!({
            "need": "n1",
            "revision": 1,
            "verdict": "units",
            "units": [{
                "id": "n1-1",
                "brief": "first pass",
                "write_scope": ["src/a.rs"],
                "proof": ["cargo test --lib a b"]
            }]
        }),
    );
    ingest(&ctx, &mut st);
    assert!(st.units.is_empty());
    assert!(st.order.is_empty());
    assert_eq!(st.needs["n1"].verdict, "invalid");
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "EXCEPTION bad-plan bad.json")
    );
    let exc = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(
        exc.contains("bad-plan bad.json unit n1-1: cargo test takes one TESTNAME filter"),
        "{exc}"
    );
}

#[test]
fn a_revision_with_an_invalid_proof_command_is_rejected_and_keeps_the_unit() {
    let (_dir, ctx, host, mut st) = setup();
    write_plan(&ctx, "n1.json", &first_plan());
    ingest(&ctx, &mut st);
    let mut bad = plan_unit("n1-1", "first pass");
    bad["proof"] = json!(["cargo test --lib a b"]);
    write_plan(
        &ctx,
        "n1.r2.json",
        &json!({"need": "n1", "revision": 2, "units": [bad]}),
    );
    ingest(&ctx, &mut st);
    assert_eq!(st.needs["n1"].revision, 1);
    assert!(st.units["n1-1"].proof.is_empty());
    assert!(host.logs().iter().any(|l| l == "EXCEPTION bad-plan n1"));
    let exc = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(
        exc.contains("bad-plan n1 unit n1-1: cargo test takes one TESTNAME filter"),
        "{exc}"
    );
}

#[test]
fn a_revision_amends_pending_and_keeps_merged_units() {
    let (_dir, ctx, _host, mut st) = setup();
    write_plan(&ctx, "n1.json", &first_plan());
    ingest(&ctx, &mut st);
    assert_eq!(st.needs["n1"].revision, 1);
    st.units.get_mut("n1-1").unwrap().status = Status::Merged;

    write_plan(
        &ctx,
        "n1.r2.json",
        &json!({
            "need": "n1",
            "revision": 2,
            "verdict": "units",
            "reason": "add the second half",
            "units": [plan_unit("n1-1", "first pass"), plan_unit("n1-2", "second half")]
        }),
    );
    ingest(&ctx, &mut st);
    assert_eq!(st.needs["n1"].revision, 2);
    assert_eq!(st.units["n1-1"].status, Status::Merged);
    assert_eq!(st.units["n1-2"].status, Status::Pending);
    assert_eq!(st.units["n1-2"].revision, 2);
    assert_eq!(st.needs["n1"].units, vec!["n1-1", "n1-2"]);
}

#[test]
fn a_revision_amends_a_needs_replan_unit_back_to_pending() {
    let (_dir, ctx, _host, mut st) = setup();
    write_plan(&ctx, "n1.json", &first_plan());
    ingest(&ctx, &mut st);
    st.units.get_mut("n1-1").unwrap().status = Status::NeedsReplan;

    let mut fixed = plan_unit("n1-1", "first pass");
    fixed["proof"] = json!(["cargo test --lib pump::state"]);
    write_plan(
        &ctx,
        "n1.r2.json",
        &json!({
            "need": "n1",
            "revision": 2,
            "verdict": "units",
            "reason": "fix the proof command",
            "units": [fixed]
        }),
    );
    ingest(&ctx, &mut st);
    assert_eq!(st.units["n1-1"].status, Status::Pending);
    assert_eq!(st.units["n1-1"].proof, vec!["cargo test --lib pump::state"]);
    assert_eq!(st.units["n1-1"].revision, 2);
}

#[test]
fn a_revision_cannot_change_a_merged_unit() {
    let (_dir, ctx, _host, mut st) = setup();
    write_plan(&ctx, "n1.json", &first_plan());
    ingest(&ctx, &mut st);
    st.units.get_mut("n1-1").unwrap().status = Status::Merged;

    write_plan(
        &ctx,
        "n1.r2.json",
        &json!({
            "need": "n1",
            "revision": 2,
            "units": [plan_unit("n1-1", "different work")]
        }),
    );
    ingest(&ctx, &mut st);
    assert_eq!(st.units["n1-1"].brief, "first pass");
    let exc = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exc.contains("revision-conflict n1-1"), "{exc}");
}

#[test]
fn a_revision_withdraws_only_named_pending_units() {
    let (_dir, ctx, _host, mut st) = setup();
    write_plan(
        &ctx,
        "n1.json",
        &json!({
            "need": "n1",
            "revision": 1,
            "units": [plan_unit("n1-1", "one"), plan_unit("n1-2", "two")]
        }),
    );
    ingest(&ctx, &mut st);
    write_plan(
        &ctx,
        "n1.r2.json",
        &json!({
            "need": "n1",
            "revision": 2,
            "units": [plan_unit("n1-1", "one"), plan_unit("n1-2", "two")],
            "withdraw": ["n1-1"]
        }),
    );
    ingest(&ctx, &mut st);
    assert_eq!(st.units["n1-1"].status, Status::Withdrawn);
    assert_eq!(st.needs["n1"].units, vec!["n1-2"]);
}

#[test]
fn a_stale_or_duplicate_revision_is_ignored() {
    let (_dir, ctx, _host, mut st) = setup();
    write_plan(&ctx, "n1.json", &first_plan());
    ingest(&ctx, &mut st);
    write_plan(
        &ctx,
        "n1.r2.json",
        &json!({"need": "n1", "revision": 2, "units": [plan_unit("n1-2", "two")]}),
    );
    ingest(&ctx, &mut st);
    assert_eq!(st.needs["n1"].revision, 2);
    write_plan(
        &ctx,
        "n1.r2b.json",
        &json!({"need": "n1", "revision": 2, "units": [plan_unit("n1-3", "three")]}),
    );
    ingest(&ctx, &mut st);
    assert!(!st.units.contains_key("n1-3"));
    assert_eq!(st.needs["n1"].revision, 2);
}
