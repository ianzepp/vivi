//! Tests for [`crate::pump::state`].

use std::collections::BTreeMap;
use std::fs;

use serde_json::json;
use tempfile::tempdir;

use crate::pump::jobs::{JobSpec, JobStatus};
use crate::pump::state::{
    Board, MergeResult, NeedRecord, Promotion, State, Status, Unit, with_target_state,
};

#[test]
fn restart_keeps_merging_units_owned_by_dispatched_merge_jobs_only() {
    let mut state = State::default();
    for uid in ["u-live", "u-orphan"] {
        state.units.insert(
            uid.to_owned(),
            Unit {
                need: String::from("n1"),
                status: Status::Merging,
                ..Unit::default()
            },
        );
    }
    let mut spec = JobSpec::new(
        String::from("merge-n1"),
        String::from("merge"),
        String::from("merge"),
        String::from("merge n1"),
        String::from("merge u-live"),
    );
    spec.units.push(String::from("u-live"));
    crate::pump::jobs::enqueue(&mut state, spec).unwrap();
    state.jobs.get_mut("merge-n1").unwrap().status = JobStatus::Dispatched;

    state.recover_after_restart();

    assert_eq!(state.units["u-live"].status, Status::Merging);
    assert_eq!(state.units["u-orphan"].status, Status::Delivered);
}

/// A `state.json` as the pump writes it: one-space indent, `null`s
/// for absent models, `baseline` null before the first gate, `rr` an int.
/// Units `2aab941d-1` (merged) and `2499da76-1` (no-defect) are copied from
/// a real pump state.
const PUMP_PY_STATE: &str = r#"{
 "units": {
  "2aab941d-1": {
   "need": "2aab941d",
   "title": "sexp: tag dispatched genus constructs and emit TypeCheck on interface/union operands",
   "brief": "Need 2aab941d (sexp half). Today `faber emit -t sexp corpus/implendum/implendum-dispatch.fab` fails with `MIR-to-sexp unsupported: runtime_call TypeCheck`.",
   "write_scope": [
    "crates/radix-mir-sexp/src/emit.rs",
    "crates/radix-mir-sexp/src/capability.rs",
    "crates/radix-mir-sexp/src/lib.rs",
    "crates/radix-mir-sexp/src/interface_dispatch_test.rs"
   ],
   "after": [],
   "proof": ["cargo test -p radix-mir-sexp --lib interface_dispatch"],
   "tools": "plain",
   "status": "merged",
   "attempts": 1,
   "history": [],
   "task": "9d98468e",
   "branch": "unit/2aab941d-1-a1",
   "model": "zai/glm-5.3-flash",
   "since": 1791939851,
   "commit": "d7a92a344a6c8c163e8b6c968bae265f4781c506",
   "outside": []
  },
  "2499da76-1": {
   "need": "2499da76",
   "title": "module: own the locale completeness diagnostic rows",
   "brief": "Need 2499da76. cargo test -p radix-module --lib locale_pack_completeness_packs_own_every_en_diagnostic_row fails after merge.",
   "write_scope": ["crates/radix-module/src/mir/lower/package_instantiate.rs"],
   "after": [],
   "proof": ["cargo test -p radix-module --lib locale_pack_completeness_packs_own_every_en_diagnostic_row"],
   "tools": "full",
   "status": "no-defect",
   "attempts": 2,
   "history": [
    {
     "attempt": 1,
     "model": "zai/glm-5.3-flash",
     "why": "proof-failed",
     "detail": "After merging into the integration branch, this command failed (exit 101):\ncargo test -p radix-module --lib locale_pack_completeness_packs_own_every_en_diagnostic_row",
     "at": 1791900000
    },
    {
     "attempt": 2,
     "model": null,
     "why": "agent-replied",
     "detail": "verdict: no defect in this unit; the read is guarded",
     "at": 1791900900
    }
   ],
   "task": "2499da76-t1",
   "branch": "unit/2499da76-1-a2",
   "model": null,
   "since": 1791899000,
   "since_dispatch": 1791899500,
   "pool": "flyfull",
   "outside": ["crates/radix-module/src/locale.rs"],
   "replaced_by": [],
   "verdict_text": "no-defect: the read is guarded by the feature flag"
  }
 },
 "order": ["2aab941d-1", "2499da76-1"],
 "needs": {
  "c8c37615": {
   "verdict": "units",
   "reason": "Cut into 12 units covering every non-test FABER_*/RADIX_* read named in the ruling.",
   "units": ["c8c37615-1", "c8c37615-2"]
  }
 },
 "merges": 371,
 "last_gate": {
  "at": 1791982127,
  "merges": 371,
  "task": null,
  "tip": "448d8115989e37402ab1606fa0331806e646fcb1",
  "pool": "flyfull"
 },
 "baseline": null,
 "closed_needs": ["b9ae0618", "e03fba11"],
 "gates": [
  {
   "tip": "0000000000000000000000000000000000000000",
   "at": 1791900000,
   "merges": 350,
   "names": null
  },
  {
   "tip": "448d8115989e37402ab1606fa0331806e646fcb1",
   "at": 1791982970,
   "merges": 371,
   "names": ["radix-module codegen::ts::tests::delimited_positions_drop_only_redundant_parentheses"]
  }
 ],
 "plans_seen": ["84eaff6a.json", "9fec06b7.json"],
 "board": {
  "at": 1791985832,
  "planning": {},
  "replied": ["f9d8a611:planned", "271b6ee7:planned", "5d445eb0:planned"]
 },
 "red": {
  "radix-module codegen::ts::tests::delimited_positions_drop_only_redundant_parentheses": "pumpfix-g448d8115"
 },
 "rr": 17
}"#;

fn write_state(dir: &std::path::Path, name: &str, text: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, text).unwrap();
    path
}

#[test]
fn a_state_written_by_pump_py_loads() {
    let dir = tempdir().unwrap();
    let path = write_state(dir.path(), "state.json", PUMP_PY_STATE);
    let st = State::load(&path).unwrap();

    let merged = &st.units["2aab941d-1"];
    assert_eq!(merged.need, "2aab941d");
    assert_eq!(merged.status, Status::Merged);
    assert_eq!(merged.attempts, 1);
    assert_eq!(merged.since, 1_791_939_851);
    assert_eq!(merged.model.as_deref(), Some("zai/glm-5.3-flash"));
    assert_eq!(merged.tools, "plain");
    assert_eq!(merged.write_scope[0], "crates/radix-mir-sexp/src/emit.rs");
    assert_eq!(
        merged.proof,
        vec!["cargo test -p radix-mir-sexp --lib interface_dispatch"]
    );
    assert!(merged.after.is_empty());
    assert_eq!(
        merged.commit.as_deref(),
        Some("d7a92a344a6c8c163e8b6c968bae265f4781c506")
    );

    let replied = &st.units["2499da76-1"];
    assert_eq!(replied.status, Status::NoDefect);
    assert_eq!(replied.pool.as_deref(), Some("flyfull"));
    assert_eq!(replied.since_dispatch, Some(1_791_899_500));
    assert_eq!(replied.history.len(), 2);
    assert_eq!(replied.history[0].why, "proof-failed");
    assert_eq!(replied.history[0].at, 1_791_900_000);
    assert_eq!(replied.history[1].model, None);
    assert_eq!(replied.outside, vec!["crates/radix-module/src/locale.rs"]);
    assert_eq!(
        replied.verdict_text.as_deref(),
        Some("no-defect: the read is guarded by the feature flag")
    );

    assert_eq!(st.order, vec!["2aab941d-1", "2499da76-1"]);
    let need = &st.needs["c8c37615"];
    assert_eq!(need.verdict, "units");
    assert_eq!(need.units, vec!["c8c37615-1", "c8c37615-2"]);
    assert_eq!(st.merges, 371);
    assert_eq!(st.baseline, None);
    assert_eq!(st.last_gate.task, None);
    assert_eq!(st.last_gate.pool.as_deref(), Some("flyfull"));
    assert_eq!(st.last_gate.merges, 371);
    assert_eq!(st.gates.len(), 2);
    assert!(st.gates[0].names.is_none());
    assert!(st.gates[1].new.is_none());
    assert_eq!(
        st.gates[1].names,
        Some(vec![
            "radix-module codegen::ts::tests::delimited_positions_drop_only_redundant_parentheses"
                .to_owned()
        ])
    );
    assert!(st.board.planning.is_empty());
    assert_eq!(st.board.replied[0], "f9d8a611:planned");
    assert_eq!(
        st.red["radix-module codegen::ts::tests::delimited_positions_drop_only_redundant_parentheses"],
        "pumpfix-g448d8115"
    );
    assert_eq!(st.closed_needs, vec!["b9ae0618", "e03fba11"]);
    assert_eq!(st.plans_seen, vec!["84eaff6a.json", "9fec06b7.json"]);
    assert!(st.promotions.is_empty());
    assert_eq!(st.extra.get("rr"), Some(&json!(17)));
}

#[test]
fn board_loads_old_shape_and_round_trips_new_fields() {
    let old: Board = serde_json::from_str(r#"{"planning":{"n1":123}}"#).unwrap();
    assert_eq!(old.planning.get("n1"), Some(&123));
    assert!(old.planner_pids.is_empty());
    assert!(old.planner_fails.is_empty());
    assert!(old.planner_retry_at.is_empty());
    assert!(old.planner_failed.is_empty());

    let board = Board {
        planner_pids: BTreeMap::from([("n1".to_string(), 4242)]),
        planner_fails: BTreeMap::from([("n1".to_string(), 2)]),
        planner_retry_at: BTreeMap::from([("n1".to_string(), 1_800_000_000)]),
        planner_failed: vec!["n2".to_string()],
        ..Board::default()
    };

    let text = serde_json::to_string(&board).unwrap();
    let back: Board = serde_json::from_str(&text).unwrap();
    assert_eq!(back.planner_pids.get("n1"), Some(&4242));
    assert_eq!(back.planner_fails.get("n1"), Some(&2));
    assert_eq!(back.planner_retry_at.get("n1"), Some(&1_800_000_000));
    assert_eq!(back.planner_failed, vec!["n2"]);
}

#[test]
fn a_missing_file_loads_as_the_empty_state() {
    let dir = tempdir().unwrap();
    let st = State::load(&dir.path().join("state.json")).unwrap();
    assert!(st.units.is_empty());
    assert!(st.order.is_empty());
    assert!(st.needs.is_empty());
    assert_eq!(st.merges, 0);
    assert_eq!(st.baseline, None);
    assert_eq!(st.last_gate.at, 0);
    assert_eq!(st.last_gate.task, None);
    assert_eq!(st.last_gate.tip, None);
    assert!(st.gates.is_empty());
    assert!(st.plans_seen.is_empty());
    assert!(st.red.is_empty());
    assert!(st.promotions.is_empty());
    assert!(st.extra.is_empty());
}

#[test]
fn a_unit_left_merging_becomes_delivered_on_load() {
    let dir = tempdir().unwrap();
    let raw = json!({
        "units": {"2499da76-2": {"need": "2499da76", "status": "merging"}},
        "order": ["2499da76-2"]
    });
    let path = write_state(dir.path(), "state.json", &raw.to_string());
    let st = State::load(&path).unwrap();
    assert_eq!(st.units["2499da76-2"].status, Status::Delivered);
}

#[test]
fn an_empty_plans_seen_is_rebuilt_from_the_needs() {
    let dir = tempdir().unwrap();
    let raw = json!({
        "units": {},
        "needs": {
            "9fec06b7": {"verdict": "units", "reason": "one unit", "units": ["9fec06b7-1"]},
            "84eaff6a": {"verdict": "units", "reason": "two units", "units": ["84eaff6a-1", "84eaff6a-2"]}
        }
    });
    let path = write_state(dir.path(), "state.json", &raw.to_string());
    let st = State::load(&path).unwrap();
    assert_eq!(st.plans_seen, vec!["84eaff6a.json", "9fec06b7.json"]);
}

#[test]
fn save_then_load_round_trips_and_leaves_no_tmp_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("nested").join("state.json");
    let mut st = State::default();
    st.units.insert(
        "2aab941d-1".to_owned(),
        Unit {
            need: "2aab941d".to_owned(),
            title: "sexp: tag dispatched genus constructs".to_owned(),
            write_scope: vec!["crates/radix-mir-sexp/src/emit.rs".to_owned()],
            tools: "plain".to_owned(),
            status: Status::Merged,
            since: 1_791_939_851,
            ..Unit::default()
        },
    );
    st.order.push("2aab941d-1".to_owned());
    st.merges = 371;
    st.baseline = Some(vec!["old failing test".to_owned()]);
    st.promotions.push(Promotion {
        tip: "448d8115989e37402ab1606fa0331806e646fcb1".to_owned(),
        at: 1_791_982_970,
        failing: None,
    });
    st.extra.insert("rr".to_owned(), json!(18));

    st.save(&path).unwrap();
    assert!(!dir.path().join("nested").join("state.json.tmp").exists());

    let back = State::load(&path).unwrap();
    let unit = &back.units["2aab941d-1"];
    assert_eq!(unit.status, Status::Merged);
    assert_eq!(unit.title, "sexp: tag dispatched genus constructs");
    assert_eq!(unit.since, 1_791_939_851);
    assert_eq!(back.order, vec!["2aab941d-1"]);
    assert_eq!(back.merges, 371);
    assert_eq!(back.baseline, Some(vec!["old failing test".to_owned()]));
    assert_eq!(back.promotions[0].at, 1_791_982_970);
    assert_eq!(back.extra.get("rr"), Some(&json!(18)));

    let text = fs::read_to_string(&path).unwrap();
    assert!(
        text.starts_with("{\n \"units\": {\n  \"2aab941d-1\": {"),
        "one-space indent: {text:.40}"
    );
}

#[test]
fn target_gate_state_isolated_from_workspace_default_and_persists() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut state = State::default();
    state.baseline = Some(vec![String::from("default failure")]);
    with_target_state(&mut state, "tela", |target| {
        target.baseline = Some(vec![String::from("tela failure")]);
        target.merges = 7;
        target
            .extra
            .insert(String::from("replans"), json!({"failure": 1}));
    });
    assert_eq!(state.baseline, Some(vec![String::from("default failure")]));
    assert_eq!(
        state.target_states["tela"].baseline,
        Some(vec![String::from("tela failure")])
    );
    assert_eq!(state.target_states["tela"].merges, 7);
    assert!(state.extra.is_empty());
    assert_eq!(
        state.target_states["tela"].extra["replans"],
        json!({"failure": 1})
    );
    state.save(&path).unwrap();
    let loaded = State::load(&path).unwrap();
    assert_eq!(
        loaded.target_states["tela"].baseline,
        Some(vec![String::from("tela failure")])
    );
    assert_eq!(
        loaded.target_states["tela"].extra["replans"],
        json!({"failure": 1})
    );
}

#[test]
fn unknown_top_level_and_unit_keys_survive_a_round_trip() {
    let dir = tempdir().unwrap();
    let raw = json!({
        "units": {"e2133f3f-3": {"need": "e2133f3f", "status": "pending", "lane": "full"}},
        "rr": 17,
        "future_counter": {"a": 1}
    });
    let path = write_state(dir.path(), "state.json", &raw.to_string());
    let st = State::load(&path).unwrap();
    assert_eq!(st.extra.get("rr"), Some(&json!(17)));
    assert_eq!(
        st.units["e2133f3f-3"].extra.get("lane"),
        Some(&json!("full"))
    );

    let out = dir.path().join("out.json");
    st.save(&out).unwrap();
    let back = State::load(&out).unwrap();
    assert_eq!(back.extra.get("rr"), Some(&json!(17)));
    assert_eq!(back.extra.get("future_counter"), Some(&json!({"a": 1})));
    assert_eq!(
        back.units["e2133f3f-3"].extra.get("lane"),
        Some(&json!("full"))
    );
}

#[test]
fn status_names_are_kebab_case_including_no_defect() {
    assert_eq!(Status::Pending.as_str(), "pending");
    assert_eq!(Status::Inflight.as_str(), "inflight");
    assert_eq!(Status::Delivered.as_str(), "delivered");
    assert_eq!(Status::Merging.as_str(), "merging");
    assert_eq!(Status::Merged.as_str(), "merged");
    assert_eq!(Status::NoDefect.as_str(), "no-defect");
    assert_eq!(Status::Stuck.as_str(), "stuck");
    assert_eq!(Status::Splitting.as_str(), "splitting");
    assert_eq!(Status::Replaced.as_str(), "replaced");

    let st: State = serde_json::from_value(json!({
        "units": {
            "u1": {"status": "no-defect"},
            "u2": {"status": "inflight"}
        }
    }))
    .unwrap();
    assert_eq!(st.units["u1"].status, Status::NoDefect);
    assert_eq!(st.units["u2"].status, Status::Inflight);
    assert_eq!(st.units["u1"].status.as_str(), "no-defect");
}

#[test]
fn a_needs_replan_unit_round_trips_through_serde() {
    let st: State = serde_json::from_value(json!({
        "units": {"u1": {"status": "needs-replan"}}
    }))
    .unwrap();
    assert_eq!(st.units["u1"].status, Status::NeedsReplan);
    assert_eq!(st.units["u1"].status.as_str(), "needs-replan");

    let text = serde_json::to_string(&st).unwrap();
    let back: State = serde_json::from_str(&text).unwrap();
    assert_eq!(back.units["u1"].status, Status::NeedsReplan);
    assert_eq!(back.units["u1"].status.as_str(), "needs-replan");
}

#[test]
fn counts_groups_units_by_status_name() {
    let st: State = serde_json::from_value(json!({
        "units": {
            "a": {"status": "pending"},
            "b": {"status": "pending"},
            "c": {"status": "inflight"},
            "d": {"status": "no-defect"}
        }
    }))
    .unwrap();
    let counts = st.counts();
    assert_eq!(counts.get("pending"), Some(&2));
    assert_eq!(counts.get("inflight"), Some(&1));
    assert_eq!(counts.get("no-defect"), Some(&1));
    assert_eq!(counts.get("merged"), None);
    assert_eq!(counts.len(), 3);
}

#[test]
fn a_merge_result_serialises_without_its_empty_optionals() {
    let landed = MergeResult {
        uid: "2aab941d-1".to_owned(),
        ok: Some(true),
        tip: Some("448d8115989e37402ab1606fa0331806e646fcb1".to_owned()),
        secs: Some(91),
        ..MergeResult::default()
    };
    assert_eq!(
        serde_json::to_value(&landed).unwrap(),
        json!({
            "uid": "2aab941d-1",
            "ok": true,
            "outside": [],
            "tip": "448d8115989e37402ab1606fa0331806e646fcb1",
            "secs": 91
        })
    );

    let failed = MergeResult {
        uid: "d381ae56-4".to_owned(),
        ok: Some(false),
        why: Some("proof-failed".to_owned()),
        detail: Some("exit 101\ncargo test -p radix-module --lib".to_owned()),
        ..MergeResult::default()
    };
    assert_eq!(
        serde_json::to_value(&failed).unwrap(),
        json!({
            "uid": "d381ae56-4",
            "ok": false,
            "outside": [],
            "why": "proof-failed",
            "detail": "exit 101\ncargo test -p radix-module --lib"
        })
    );

    let unknown = MergeResult {
        uid: "84e6122f-10.1".to_owned(),
        ..MergeResult::default()
    };
    assert_eq!(
        serde_json::to_value(&unknown).unwrap(),
        json!({"uid": "84e6122f-10.1", "ok": null, "outside": []})
    );
}

/// A trimmed copy of the live pump state (1325 units cut to 19, one or two
/// of every status including three hand-set `withdrawn`), written by the pump.
const LIVE_SNAPSHOT: &str = include_str!("state_snapshot.json");

fn status_map(st: &State) -> Vec<(String, String)> {
    st.units
        .iter()
        .map(|(uid, unit)| (uid.clone(), unit.status.as_str().to_owned()))
        .collect()
}

#[test]
fn the_live_snapshot_round_trips_through_load_and_save() {
    let dir = tempdir().unwrap();
    let first = write_state(dir.path(), "state.json", LIVE_SNAPSHOT);
    let before = State::load(&first).unwrap();
    assert_eq!(before.units.len(), 19);
    assert_eq!(before.counts().get("withdrawn"), Some(&3));

    let second = dir.path().join("saved.json");
    before.save(&second).unwrap();
    let after = State::load(&second).unwrap();
    assert_eq!(after.units.len(), before.units.len());
    assert_eq!(status_map(&after), status_map(&before));
    assert_eq!(after.counts(), before.counts());

    // The JSON itself is unchanged apart from the merging -> delivered fold
    // and the empty `outside` / `replaced_by` lists the model always writes.
    let mut want: serde_json::Value = serde_json::from_str(LIVE_SNAPSHOT).unwrap();
    for unit in want["units"].as_object_mut().unwrap().values_mut() {
        if unit["status"] == "merging" {
            unit["status"] = json!("delivered");
        }
        for key in ["outside", "replaced_by"] {
            unit.as_object_mut()
                .unwrap()
                .entry(key)
                .or_insert(json!([]));
        }
        unit.as_object_mut()
            .unwrap()
            .entry("target")
            .or_insert(json!("default"));
        unit.as_object_mut()
            .unwrap()
            .entry("transient_retries")
            .or_insert(json!(0));
    }
    let got: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&second).unwrap()).unwrap();
    assert_eq!(got, want);
}

#[test]
fn an_unknown_status_loads_and_is_written_back_verbatim() {
    let dir = tempdir().unwrap();
    let text = r#"{"units": {"a-1": {"status": "parked-by-hand", "need": "a"}}}"#;
    let path = write_state(dir.path(), "state.json", text);
    let st = State::load(&path).unwrap();
    assert_eq!(st.units["a-1"].status.as_str(), "parked-by-hand");
    assert_eq!(st.counts().get("parked-by-hand"), Some(&1));

    st.save(&path).unwrap();
    let saved: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["units"]["a-1"]["status"], "parked-by-hand");
    // The same string interns to an equal status.
    assert_eq!(st.units["a-1"].status, Status::from_name("parked-by-hand"));
    assert_eq!(Status::from_name("withdrawn"), Status::Withdrawn);
}

#[test]
fn imported_legacy_revisions_become_one() {
    let mut st = State::default();
    st.needs.insert(
        String::from("n"),
        NeedRecord {
            verdict: String::from("units"),
            ..NeedRecord::default()
        },
    );
    st.units.insert(String::from("u"), Unit::default());
    st.normalize_legacy();
    assert_eq!(st.needs["n"].revision, 1);
    assert_eq!(st.units["u"].revision, 1);
    // A real revision is left alone.
    st.needs.get_mut("n").unwrap().revision = 3;
    st.normalize_legacy();
    assert_eq!(st.needs["n"].revision, 3);
}

#[test]
fn absent_optional_unit_fields_stay_absent_on_save() {
    let dir = tempdir().unwrap();
    let text = r#"{"units": {"a-1": {"status": "pending", "need": "a", "task": null, "branch": null, "model": null}}}"#;
    let path = write_state(dir.path(), "state.json", text);
    let st = State::load(&path).unwrap();
    st.save(&path).unwrap();
    let saved: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let unit = saved["units"]["a-1"].as_object().unwrap();
    for key in ["commit", "pool", "verdict_text", "since_dispatch"] {
        assert!(!unit.contains_key(key), "{key} must stay absent");
    }
    // consumers index these directly, so they stay present as null.
    for key in ["task", "branch", "model"] {
        assert!(unit[key].is_null(), "{key} must stay null");
    }
}

#[test]
fn transient_retry_fields_round_trip_and_default() {
    let mut unit = Unit::default();
    unit.transient_retries = 2;
    unit.retry_not_before = Some(99);
    let back: Unit = serde_json::from_str(&serde_json::to_string(&unit).unwrap()).unwrap();
    assert_eq!(back.transient_retries, 2);
    assert_eq!(back.retry_not_before, Some(99));

    // A unit from an older pump has neither key; both load as defaults.
    let old: Unit = serde_json::from_str(r#"{"need": "n", "status": "pending"}"#).unwrap();
    assert_eq!(old.transient_retries, 0);
    assert_eq!(old.retry_not_before, None);
}

#[test]
fn split_planner_fields_round_trip_and_default() {
    let mut unit = Unit::default();
    unit.split_fails = 2;
    unit.split_retry_at = Some(99);
    unit.split_reasks = 1;
    let back: Unit = serde_json::from_str(&serde_json::to_string(&unit).unwrap()).unwrap();
    assert_eq!(back.split_fails, 2);
    assert_eq!(back.split_retry_at, Some(99));
    assert_eq!(back.split_reasks, 1);

    // A unit from an older pump has none of the keys; all load as defaults.
    let old: Unit = serde_json::from_str(r#"{"need": "n", "status": "pending"}"#).unwrap();
    assert_eq!(old.split_fails, 0);
    assert_eq!(old.split_retry_at, None);
    assert_eq!(old.split_reasks, 0);
}

#[test]
fn a_withdrawn_or_unknown_unit_does_not_satisfy_after() {
    use crate::pump::ready::satisfied;

    let mut st = State::default();
    for (uid, status) in [
        ("w-1", Status::Withdrawn),
        ("o-1", Status::from_name("parked-by-hand")),
    ] {
        st.units.insert(
            uid.to_owned(),
            Unit {
                status,
                ..Unit::default()
            },
        );
        assert!(!satisfied(&st, uid, 0), "{uid} must not satisfy after");
    }

    // A replaced unit whose replacement is withdrawn is unsatisfied too.
    st.units.insert(
        "r-1".to_owned(),
        Unit {
            status: Status::Replaced,
            replaced_by: vec!["w-1".to_owned()],
            ..Unit::default()
        },
    );
    assert!(!satisfied(&st, "r-1", 0));
}

#[test]
fn target_merge_count_excludes_unaccepted_need_branches_before_extra_swap() {
    let mut state = State::default();
    let unit = Unit {
        need: "n1".into(),
        target: "tela".into(),
        status: Status::Merged,
        ..Unit::default()
    };
    state.units.insert("u1".into(), unit);
    crate::pump::need::reserve(&mut state, "tela", "n1", 1, &"a".repeat(40)).unwrap();
    state.units.insert(
        "legacy".into(),
        Unit {
            need: "legacy".into(),
            target: "tela".into(),
            status: Status::Merged,
            ..Unit::default()
        },
    );
    with_target_state(&mut state, "tela", |scoped| {
        assert_eq!(scoped.merges, 1);
        assert!(
            crate::pump::need::get(scoped, "tela", "n1")
                .unwrap()
                .is_some()
        );
        scoped.extra.get_mut("need_branches").unwrap()["tela"]["n1"]["error"] =
            json!("observed while scoped");
    });
    assert_eq!(
        crate::pump::need::get(&state, "tela", "n1")
            .unwrap()
            .unwrap()
            .error
            .as_deref(),
        Some("observed while scoped")
    );
    assert!(
        !state.target_states["tela"]
            .extra
            .contains_key("need_branches")
    );
    assert!(
        crate::pump::need::get(&state, "tela", "n1")
            .unwrap()
            .is_some()
    );
    state
        .extra
        .insert("need_branches".into(), json!({"tela": {"n1": "corrupt"}}));
    with_target_state(&mut state, "tela", |scoped| assert_eq!(scoped.merges, 1));
}
