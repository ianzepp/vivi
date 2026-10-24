//! Tests for [`super`]: the deterministic plan checks and what the ingest and
//! the board do with their findings.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use super::{absence_only, appears, test_filters, vacuous_proof};
use crate::pump::board::{NeedRow, replan_unblocked};
use crate::pump::config::{PlanConfig, ProofConfig, ProofRule, PumpConfig};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::plan::ingest;
use crate::pump::state::{NeedRecord, State, Status, Unit};

fn setup_with(cfg: PumpConfig) -> (TempDir, Ctx, Arc<FakeHost>, State) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg);
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    (dir, ctx, host, State::default())
}

fn setup() -> (TempDir, Ctx, Arc<FakeHost>, State) {
    setup_with(PumpConfig::default())
}

fn write_plan(ctx: &Ctx, name: &str, plan: &Value) {
    fs::write(ctx.home.join("plans").join(name), plan.to_string()).unwrap();
}

fn unit(id: &str, scope: &[&str], proof: &[&str]) -> Value {
    json!({
        "id": id,
        "title": id,
        "brief": "change the thing",
        "write_scope": scope,
        "proof": proof,
        "tools": "plain"
    })
}

fn plan(need: &str, units: &[Value]) -> Value {
    json!({"need": need, "revision": 1, "verdict": "units", "reason": "r", "units": units})
}

fn strs(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

// --- vacuous proofs ----------------------------------------------------

#[test]
fn negated_searches_and_count_zero_pipelines_are_absence_only() {
    for command in [
        "! grep -q old src/a.rs",
        "! rg old src",
        "! git grep -q old -- src",
        "grep -c old src/a.rs | grep -q '^0$'",
        "grep -rc old src | grep -qx 0",
        "sh -c '! grep -q old src/a.rs'",
        "! grep -q one src/a.rs && ! grep -q two src/a.rs",
    ] {
        assert!(absence_only(command, 0), "{command}");
    }
}

#[test]
fn proofs_that_can_fail_for_the_change_are_not_absence_only() {
    for command in [
        "cargo test -p x --lib new_behavior",
        "grep -q new_symbol src/a.rs",
        "! grep -q old src/a.rs && cargo test new_behavior",
        "! grep -q old src/a.rs || true",
        "grep -c old src/a.rs | wc -l",
        "grep -c old src/a.rs",
        "test -f src/a.rs",
        "",
    ] {
        assert!(!absence_only(command, 0), "{command}");
    }
}

#[test]
fn a_unit_is_vacuous_only_when_every_proof_is_absence_only() {
    assert!(vacuous_proof(&strs(&["! grep -q old src/a.rs"])).is_some());
    assert!(
        vacuous_proof(&strs(&["! grep -q old a", "! rg gone src"]))
            .unwrap()
            .contains("only checks that something is absent")
    );
    assert!(vacuous_proof(&strs(&["! grep -q old a", "cargo test t"])).is_none());
    assert!(vacuous_proof(&[]).is_none());
}

// --- cargo test filters ------------------------------------------------

#[test]
fn test_filters_reads_the_single_filter_of_each_cargo_test() {
    let found = test_filters(&strs(&[
        "cargo test -p radix-air --lib reverse_ad::transform_selected",
        "PATH=/opt/bin:$PATH cargo test --lib other_test -- --nocapture",
        "sh -c 'cargo test third_test'",
        "cargo test -p only_a_package",
        "cargo build",
        "cargo test --lib 'with*glob'",
        "cargo test --lib reverse_ad::transform_selected",
    ]));
    assert_eq!(
        found,
        strs(&["reverse_ad::transform_selected", "other_test", "third_test"])
    );
}

#[test]
fn a_filter_is_found_by_its_full_text_or_its_last_segment() {
    let texts = vec![String::from("fn nvvm_lane_entry() {}")];
    assert!(appears("nvvm_lane_entry", "", &texts));
    assert!(appears("nvvm::lane_test::nvvm_lane_entry", "", &texts));
    assert!(appears("lane::brief_name", "add brief_name here", &[]));
    assert!(!appears("nvvm::lane_test::other_name", "", &texts));
}

fn populate(ctx: &Ctx, rel: &str, text: &str) {
    let path = ctx.repo.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn plan_with_filter(brief: &str, scope: &[&str], filter: &str) -> Value {
    let mut u = unit("n1-1", scope, &[&format!("cargo test -p x --lib {filter}")]);
    u["brief"] = json!(brief);
    plan("n1", &[u])
}

#[test]
fn a_cargo_filter_found_nowhere_rejects_the_plan() {
    let (_dir, ctx, _host, mut st) = setup();
    populate(&ctx, "src/a.rs", "fn existing_test() {}\n");
    write_plan(
        &ctx,
        "n1.json",
        &plan_with_filter("do it", &["src/a.rs"], "no_such_test"),
    );
    ingest(&ctx, &mut st);
    let record = &st.needs["n1"];
    assert_eq!(record.verdict, "invalid");
    assert!(
        record
            .reason
            .contains("unit n1-1: `cargo test` filter `no_such_test` appears neither"),
        "{}",
        record.reason
    );
    assert!(st.units.is_empty());
}

#[test]
fn a_cargo_filter_in_the_brief_a_scope_file_or_a_scope_dir_passes() {
    for (brief, scope, name) in [
        (
            "write the test named fresh_test",
            "src/new.rs",
            "fresh_test",
        ),
        ("do it", "src/a.rs", "existing_test"),
        ("do it", "src", "existing_test"),
        ("do it", "src/*.rs", "existing_test"),
        ("do it", "./src/", "mod_a::existing_test"),
    ] {
        let (_dir, ctx, _host, mut st) = setup();
        populate(&ctx, "src/a.rs", "fn existing_test() {}\n");
        write_plan(&ctx, "n1.json", &plan_with_filter(brief, &[scope], name));
        ingest(&ctx, &mut st);
        assert_eq!(st.needs["n1"].verdict, "units", "{scope} {name}");
    }
}

#[test]
fn an_empty_checkout_or_a_too_big_scope_accepts_the_filter() {
    let (_dir, ctx, _host, mut st) = setup();
    // The checkout has no files: nothing says which tests exist.
    write_plan(
        &ctx,
        "n1.json",
        &plan_with_filter("do it", &["src/a.rs"], "no_such_test"),
    );
    ingest(&ctx, &mut st);
    assert_eq!(st.needs["n1"].verdict, "units");
}

// --- proof rules -------------------------------------------------------

fn rules_cfg() -> PumpConfig {
    PumpConfig {
        proof: ProofConfig {
            allow: strs(&["faber"]),
        },
        plan: PlanConfig {
            proof_rules: vec![ProofRule {
                scope: String::from("corpus-suites/**/case.toml"),
                proof: String::from("faber corpus .* --target"),
                reason: String::from("units that change pinned outcomes must run the corpus"),
            }],
        },
        ..PumpConfig::default()
    }
}

#[test]
fn a_unit_under_a_rule_scope_needs_a_matching_proof() {
    let (_dir, ctx, _host, mut st) = setup_with(rules_cfg());
    write_plan(
        &ctx,
        "n1.json",
        &plan(
            "n1",
            &[unit(
                "n1-1",
                &["corpus-suites/full/a/case.toml", "src/b.rs"],
                &["cargo test --lib t"],
            )],
        ),
    );
    // The cargo filter would also be unfindable only with files present; the
    // checkout is empty here, so the rule is the one finding.
    ingest(&ctx, &mut st);
    let record = &st.needs["n1"];
    assert_eq!(record.verdict, "invalid");
    assert!(
        record
            .reason
            .starts_with("unit n1-1: units that change pinned outcomes must run the corpus"),
        "{}",
        record.reason
    );
    assert!(record.reason.contains("`faber corpus .* --target`"));
    assert_eq!(
        st.board.plan_findings["n1"], record.reason,
        "the findings are kept for the next prompt"
    );
}

#[test]
fn a_matching_proof_or_an_unrelated_scope_satisfies_the_rule() {
    let (_dir, ctx, _host, mut st) = setup_with(rules_cfg());
    write_plan(
        &ctx,
        "n1.json",
        &plan(
            "n1",
            &[
                unit(
                    "n1-1",
                    &["corpus-suites/full/a/case.toml"],
                    &["faber corpus corpus-suites/full --target rust"],
                ),
                unit("n1-2", &["crates/x/src/lib.rs"], &["cargo test --lib t"]),
            ],
        ),
    );
    ingest(&ctx, &mut st);
    assert_eq!(st.needs["n1"].verdict, "units");
    assert_eq!(st.units.len(), 2);
}

#[test]
fn a_rule_without_a_reason_names_its_scope_and_proof() {
    let rule = ProofRule {
        scope: String::from("a/**"),
        proof: String::from("check"),
        reason: String::new(),
    };
    assert_eq!(
        rule.reason_text(),
        "write scope `a/**` needs a proof command matching `check`"
    );
}

// --- merged units ------------------------------------------------------

fn merged_unit(st: &mut State, id: &str, brief: &str, scope: &[&str]) {
    st.units.insert(
        id.to_string(),
        Unit {
            need: String::from("n1"),
            status: Status::Merged,
            revision: 1,
            brief: brief.to_string(),
            write_scope: strs(scope),
            ..Unit::default()
        },
    );
    st.order.push(id.to_string());
    st.needs
        .entry(String::from("n1"))
        .or_insert_with(|| NeedRecord {
            verdict: String::from("units"),
            revision: 1,
            amendment_pending: true,
            ..NeedRecord::default()
        })
        .units
        .push(id.to_string());
}

fn revision(units: &[Value]) -> Value {
    json!({"need": "n1", "revision": 2, "verdict": "units", "reason": "r", "units": units})
}

#[test]
fn a_revision_that_marks_an_unknown_unit_merged_is_rejected() {
    let (_dir, ctx, host, mut st) = setup();
    merged_unit(&mut st, "n1-1", "first", &["src/a.rs"]);
    let mut landed = unit("n1-9", &["src/z.rs"], &["cargo test --lib t"]);
    landed["status"] = json!("merged");
    write_plan(&ctx, "n1.r2.json", &revision(&[landed]));
    ingest(&ctx, &mut st);
    assert!(!st.units.contains_key("n1-9"));
    assert_eq!(st.needs["n1"].revision, 1);
    assert!(
        st.board.plan_findings["n1"].contains("marks it merged but the pump has no merged unit"),
        "{}",
        st.board.plan_findings["n1"]
    );
    // The rejection is a failed answer: counted, with a backoff to re-ask.
    assert_eq!(st.board.planner_fails["n1.revision"], 1);
    assert!(st.board.planner_retry_at.contains_key("n1.revision"));
    assert!(
        host.logs()
            .iter()
            .any(|l| l.starts_with("bad-plan n1: unit n1-9: the plan marks it merged")),
        "{:?}",
        host.logs()
    );
}

#[test]
fn a_revision_that_echoes_a_merged_unit_keeps_it_merged() {
    let (_dir, ctx, host, mut st) = setup();
    merged_unit(&mut st, "n1-1", "first", &["src/a.rs"]);
    let mut echo = unit("n1-1", &["src/a.rs"], &["cargo test --lib t"]);
    echo["brief"] = json!("first");
    echo["status"] = json!("merged");
    write_plan(&ctx, "n1.r2.json", &revision(&[echo]));
    ingest(&ctx, &mut st);
    assert_eq!(st.units["n1-1"].status, Status::Merged);
    assert_eq!(st.needs["n1"].revision, 2);
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "revision keeps n1-1 as merged; it is not dispatched again"),
        "{:?}",
        host.logs()
    );
}

#[test]
fn a_new_unit_that_repeats_a_merged_unit_is_rejected() {
    let (_dir, ctx, _host, mut st) = setup();
    merged_unit(
        &mut st,
        "n1-1",
        "change the thing",
        &["src/b.rs", "src/a.rs"],
    );
    write_plan(
        &ctx,
        "n1.r2.json",
        &revision(&[unit(
            "n1-2",
            &["src/a.rs", "src/b.rs"],
            &["cargo test --lib t"],
        )]),
    );
    ingest(&ctx, &mut st);
    assert!(!st.units.contains_key("n1-2"));
    assert!(
        st.board.plan_findings["n1"]
            .contains("unit n1-2: it repeats merged unit n1-1 (same brief and write scope)"),
        "{}",
        st.board.plan_findings["n1"]
    );
}

// --- the reason is always logged ----------------------------------------

#[test]
fn every_rejection_logs_the_reason() {
    let (_dir, ctx, host, mut st) = setup();
    write_plan(
        &ctx,
        "bad.json",
        &json!({"need": "n1", "verdict": "units", "units": [{"id": "n1-1"}]}),
    );
    ingest(&ctx, &mut st);
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "bad-plan bad.json: unit missing id, brief or write_scope"),
        "{:?}",
        host.logs()
    );
}

// --- re-ask a rejected first plan --------------------------------------

fn row(handle: &str) -> NeedRow {
    NeedRow {
        handle: handle.to_string(),
        date: String::from("2026-10-22"),
        subject: String::from("subject"),
        marks: Vec::new(),
    }
}

#[test]
fn an_accepted_plan_clears_the_findings_and_the_count() {
    let (_dir, ctx, _host, mut st) = setup();
    st.board
        .plan_findings
        .insert(String::from("n1"), String::from("old findings"));
    st.board.planner_fails.insert(String::from("n1.invalid"), 2);
    write_plan(
        &ctx,
        "n1.json",
        &plan(
            "n1",
            &[unit("n1-1", &["src/a.rs"], &["cargo test --lib t"])],
        ),
    );
    ingest(&ctx, &mut st);
    assert_eq!(st.needs["n1"].verdict, "units");
    assert!(st.board.plan_findings.is_empty());
    assert!(!st.board.planner_fails.contains_key("n1.invalid"));
}

#[test]
fn a_parked_design_reason_holds_at_once_without_a_re_ask() {
    let (_dir, ctx, _host, mut st) = setup_with(PumpConfig {
        targets: BTreeMap::from([(
            String::from("a"),
            crate::pump::config::PumpTarget {
                path: String::from("a"),
                ..crate::pump::config::PumpTarget::default()
            },
        )]),
        ..PumpConfig::default()
    });
    let mut u = unit("n1-1", &["src/a.rs"], &["cargo test --lib t"]);
    u["target"] = json!("nowhere");
    write_plan(&ctx, "n1.json", &plan("n1", &[u]));
    ingest(&ctx, &mut st);
    assert_eq!(st.needs["n1"].verdict, "hold");
    assert!(st.board.plan_findings.is_empty());
}

// --- needs that wait on unlanded needs ----------------------------------

fn other_need_unit(st: &mut State, id: &str, need: &str, status: Status) {
    st.units.insert(
        id.to_string(),
        Unit {
            need: need.to_string(),
            status,
            write_scope: strs(&["crates/shared"]),
            ..Unit::default()
        },
    );
    st.order.push(id.to_string());
    st.needs
        .entry(need.to_string())
        .or_insert_with(|| NeedRecord {
            verdict: String::from("units"),
            revision: 1,
            ..NeedRecord::default()
        })
        .units
        .push(id.to_string());
}

#[test]
fn a_plan_depending_on_an_unlanded_need_is_held_not_minted() {
    let (_dir, ctx, host, mut st) = setup();
    other_need_unit(&mut st, "p1-1", "p1", Status::Inflight);
    let mut dependent = unit("n1-1", &["src/a.rs"], &["cargo test --lib t"]);
    dependent["after"] = json!(["p1-1"]);
    write_plan(&ctx, "n1.json", &plan("n1", &[dependent]));
    ingest(&ctx, &mut st);
    let record = &st.needs["n1"];
    assert_eq!(record.verdict, "hold");
    assert_eq!(record.reason, "waits on p1");
    assert_eq!(record.waits_on, strs(&["p1"]));
    assert_eq!(record.units, Vec::<String>::new());
    assert!(!st.units.contains_key("n1-1"));
    assert!(host.logs().iter().any(|l| l == "plan n1 held: waits on p1"));
}

#[test]
fn a_backlog_graph_edge_to_an_unlanded_need_holds_the_plan() {
    let (_dir, ctx, host, mut st) = setup();
    host.reply(
        "graph blocked",
        0,
        r#"[{"source_id":"n1","blocked_by":["p2","p3"]}]"#,
    );
    other_need_unit(&mut st, "p3-1", "p3", Status::Merged);
    write_plan(
        &ctx,
        "n1.json",
        &plan(
            "n1",
            &[unit("n1-1", &["src/a.rs"], &["cargo test --lib t"])],
        ),
    );
    ingest(&ctx, &mut st);
    // p3 landed (all units merged); p2 is unknown to the pump, so unlanded.
    assert_eq!(st.needs["n1"].reason, "waits on p2");
    assert!(st.units.keys().all(|id| id != "n1-1"));
}

#[test]
fn a_plan_whose_prerequisites_have_landed_is_ingested() {
    let (_dir, ctx, host, mut st) = setup();
    host.reply(
        "graph blocked",
        0,
        r#"[{"source_id":"n1","blocked_by":["p1"]}]"#,
    );
    other_need_unit(&mut st, "p1-1", "p1", Status::Merged);
    let mut dependent = unit("n1-1", &["src/a.rs"], &["cargo test --lib t"]);
    dependent["after"] = json!(["p1-1"]);
    write_plan(&ctx, "n1.json", &plan("n1", &[dependent]));
    ingest(&ctx, &mut st);
    assert_eq!(st.needs["n1"].verdict, "units");
    assert!(st.units.contains_key("n1-1"));
}

#[test]
fn a_held_need_is_planned_again_when_its_prerequisites_land() {
    let (_dir, ctx, _host, mut st) = setup();
    other_need_unit(&mut st, "p1-1", "p1", Status::Inflight);
    let mut dependent = unit("n1-1", &["src/a.rs"], &["cargo test --lib t"]);
    dependent["after"] = json!(["p1-1"]);
    write_plan(&ctx, "n1.json", &plan("n1", &[dependent]));
    ingest(&ctx, &mut st);
    let rows = [row("n1")];

    replan_unblocked(&ctx, &mut st, &rows);
    assert!(st.needs.contains_key("n1"), "p1 has not landed");

    st.units.get_mut("p1-1").unwrap().status = Status::Merged;
    replan_unblocked(&ctx, &mut st, &rows);
    assert!(!st.needs.contains_key("n1"));
    assert!(!ctx.home.join("plans/n1.json").exists());
    assert!(!st.plans_seen.iter().any(|seen| seen == "n1.json"));
    assert!(ctx.home.join("plans/superseded").is_dir());
}

#[test]
fn a_hold_the_operator_set_is_not_replanned_by_landing() {
    let (_dir, ctx, _host, mut st) = setup();
    st.needs.insert(
        String::from("n1"),
        NeedRecord {
            verdict: String::from("hold"),
            reason: String::from("operator ruling needed"),
            ..NeedRecord::default()
        },
    );
    replan_unblocked(&ctx, &mut st, &[row("n1")]);
    assert!(st.needs.contains_key("n1"));
}

// --- write-scope overlap -------------------------------------------------

#[test]
fn a_new_unit_overlapping_another_needs_live_unit_is_warned() {
    let (_dir, ctx, host, mut st) = setup();
    other_need_unit(&mut st, "p1-1", "p1", Status::Pending);
    other_need_unit(&mut st, "p2-1", "p2", Status::Merged);
    write_plan(
        &ctx,
        "n1.json",
        &plan(
            "n1",
            &[
                unit(
                    "n1-1",
                    &["crates/shared/src/lib.rs"],
                    &["cargo test --lib t"],
                ),
                unit("n1-2", &["crates/other"], &["cargo test --lib t"]),
            ],
        ),
    );
    ingest(&ctx, &mut st);
    let warns: Vec<String> = host
        .logs()
        .into_iter()
        .filter(|l| l.contains("overlap-warn"))
        .collect();
    assert_eq!(warns.len(), 1, "{warns:?}");
    assert!(warns[0].contains("unit n1-1 (need n1)"), "{}", warns[0]);
    assert!(warns[0].contains("p1-1 (need p1, pending)"), "{}", warns[0]);
    assert!(!warns[0].contains("p2-1"), "a merged unit is not live");
    assert_eq!(st.units.len(), 4, "the plan is still ingested");
}

// --- config --------------------------------------------------------------

#[test]
fn proof_rules_parse_from_the_pump_table() {
    let cfg: PumpConfig = toml::from_str(
        r#"
        committer_name = "n"
        committer_email = "e@x"
        branch = "b"
        [[plan.proof_rules]]
        scope = "corpus-suites/**/case.toml"
        proof = "faber corpus .* --target"
        reason = "why"
        [[plan.proof_rules]]
        scope = "x/**"
        proof = "("
        "#,
    )
    .unwrap();
    assert_eq!(cfg.plan.proof_rules.len(), 2);
    assert_eq!(cfg.plan.proof_rules[0].reason, "why");
}

#[test]
fn an_empty_proof_rule_table_is_not_written() {
    let text = toml::to_string(&PumpConfig::default()).unwrap();
    assert!(!text.contains("proof_rules"), "{text}");
}
