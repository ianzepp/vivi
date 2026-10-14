//! Tests for [`super::plan`].

use std::fs;
use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::plan::{ingest, overlaps, scope_root};
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
fn scope_root_cuts_a_glob_at_the_last_directory() {
    assert_eq!(scope_root("src/*/lib.rs"), "src");
    assert_eq!(scope_root("src/lib/**/*.rs"), "src/lib");
    assert_eq!(scope_root("a/b/[abc].txt"), "a/b");
    assert_eq!(scope_root("a/b?/c.rs"), "a");
    assert_eq!(scope_root("*.rs"), "");
    assert_eq!(scope_root("plain/path.txt"), "plain/path.txt");
}

#[test]
fn scope_root_trims_leading_dots_and_slashes_and_the_trailing_slash() {
    assert_eq!(scope_root("./src/lib.rs"), "src/lib.rs");
    assert_eq!(scope_root(".//src/x/"), "src/x");
    assert_eq!(scope_root("../up/x"), "up/x");
    assert_eq!(scope_root(" src/x "), "src/x");
    assert_eq!(scope_root("...///src/"), "src");
    assert_eq!(scope_root("./[*]/x"), "");
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
    assert_eq!(u2.tools, "plain");
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
