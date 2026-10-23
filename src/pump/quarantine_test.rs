//! Tests for leftover quarantine state: parking units, opening a repair,
//! and closing that repair. Live merges no longer create these records.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use crate::pump::config::{DispatchConfig, Kind, Lane, MergeConfig, PumpConfig};
use crate::pump::dispatch::dispatch;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::quarantine::{branch, repair_no_defect, repair_of, repair_uid, set_aside};
use crate::pump::state::{MergeResult, NeedRecord, Quarantine, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore};

fn setup() -> (TempDir, Ctx, std::sync::Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        branch: String::from("next"),
        merge: MergeConfig {
            shared_checks: vec![String::from("cargo test --lib")],
            ..MergeConfig::default()
        },
        dispatch: DispatchConfig {
            kinds: BTreeMap::from([(String::from("plain"), Kind::default())]),
            default_kind: String::from("plain"),
            lanes: vec![Lane {
                model: String::from("m/a"),
                cap: 2,
            }],
            ..DispatchConfig::default()
        },
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    (dir, ctx, host)
}

fn state_with(units: &[(&str, Status)]) -> State {
    let mut st = State::default();
    for (uid, status) in units {
        st.order.push((*uid).to_string());
        st.units.insert(
            (*uid).to_string(),
            Unit {
                need: String::from("n1"),
                title: format!("Unit {uid}"),
                write_scope: vec![String::from("src/pump/merge.rs")],
                proof: vec![format!("./proof-{uid}.sh")],
                tools: String::from("plain"),
                status: *status,
                attempts: 1,
                branch: Some(format!("work/{uid}")),
                ..Unit::default()
            },
        );
    }
    st.needs.insert(
        String::from("n1"),
        NeedRecord {
            units: st.order.clone(),
            ..NeedRecord::default()
        },
    );
    st
}

fn quarantined(uid: &str, q: &Quarantine) -> MergeResult {
    MergeResult {
        uid: uid.to_string(),
        why: Some(String::from("quarantined")),
        detail: Some(q.id.clone()),
        quarantine: Some(q.clone()),
        ..MergeResult::default()
    }
}

fn parked_pair(ctx: &Ctx) -> (State, String) {
    let q = Quarantine {
        id: String::from("q1000000-abc123"),
        cmd: String::from("cargo test --lib"),
        code: 1,
        text: String::from("error[E0308]: mismatched types"),
        units: vec![String::from("u1"), String::from("u2")],
    };
    let mut st = state_with(&[("u1", Status::Merging), ("u2", Status::Merging)]);
    set_aside(ctx, &mut st, &quarantined("u1", &q));
    set_aside(ctx, &mut st, &quarantined("u2", &q));
    (st, q.id)
}

#[test]
fn applying_a_quarantined_batch_parks_the_units_and_opens_one_repair_unit() {
    let (_dir, ctx, _host) = setup();
    let (st, id) = parked_pair(&ctx);
    for uid in ["u1", "u2"] {
        assert_eq!(st.units[uid].status, Status::Quarantined);
        assert_eq!(st.units[uid].quarantine.as_deref(), Some(id.as_str()));
    }
    let repair = repair_uid(&ctx, &id);
    assert_eq!(repair, format!("pumpfix-{id}"));
    let unit = &st.units[&repair];
    assert_eq!(unit.status, Status::Pending);
    assert_eq!(unit.need, repair);
    assert_eq!(unit.proof, ["cargo test --lib"]);
    assert_eq!(unit.write_scope, ["src/pump/merge.rs"]);
    assert_eq!(unit.base, Some(format!("origin/{}", branch(&id))));
    assert_eq!(unit.repairs.as_deref(), Some(id.as_str()));
    assert!(unit.solo);
}

fn dispatch_cfg() -> PumpConfig {
    PumpConfig {
        branch: String::from("next"),
        merge: MergeConfig {
            shared_checks: vec![String::from("cargo test --lib")],
            ..MergeConfig::default()
        },
        dispatch: DispatchConfig {
            lanes: vec![Lane {
                model: String::from("m/a"),
                cap: 2,
            }],
            kinds: BTreeMap::from([(
                String::from("plain"),
                Kind {
                    pools: vec![String::from("fly4")],
                    ..Kind::default()
                },
            )]),
            default_kind: String::from("plain"),
            ..DispatchConfig::default()
        },
        ..PumpConfig::default()
    }
}

#[test]
fn the_repair_unit_is_dispatched_from_its_quarantine_branch() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), dispatch_cfg());
    host.reply("merge-base --is-ancestor origin/work/", 1, "");
    host.reply("pool status", 0, "slot fly4 bridge slot-1 free\n");
    host.reply("task send", 0, "created hand 1a2b3c4d\n");
    host.reply(
        "pool dispatch",
        0,
        "dispatched 1a2b3c4d bridge slot-1 m/a x",
    );
    let (mut st, id) = parked_pair(&ctx);
    dispatch(&ctx, &mut st);
    let repair = repair_uid(&ctx, &id);
    assert_eq!(st.units[&repair].status, Status::Inflight);
    let sent: Vec<String> = host
        .commands()
        .into_iter()
        .filter(|c| c.contains("pool dispatch --pool"))
        .collect();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains(&format!("--from origin/{}", branch(&id))));
    assert_eq!(st.units["u1"].status, Status::Quarantined);
}

#[test]
fn a_repair_unit_that_finds_no_defect_returns_the_units_to_delivered() {
    let (_dir, ctx, _host) = setup();
    let (mut st, id) = parked_pair(&ctx);
    let repair = repair_uid(&ctx, &id);
    repair_no_defect(&ctx, &mut st, &repair);
    for uid in ["u1", "u2"] {
        assert_eq!(st.units[uid].status, Status::Delivered);
        assert!(st.units[uid].solo);
        assert_eq!(st.units[uid].quarantine, None);
    }
}

#[test]
fn a_restart_leaves_quarantined_units_alone() {
    let mut st = state_with(&[("u1", Status::Quarantined), ("u2", Status::Merging)]);
    st.recover_after_restart();
    assert_eq!(st.units["u1"].status, Status::Quarantined);
    assert_eq!(st.units["u2"].status, Status::Delivered);
    assert_eq!(st.counts().get("quarantined"), Some(&1));
    assert_eq!(Status::from_name("quarantined"), Status::Quarantined);
    assert_eq!(Status::Quarantined.as_str(), "quarantined");
}

#[test]
fn the_quarantine_fields_round_trip_through_the_store() {
    let dir = tempdir().unwrap();
    let mut store = PumpStore::open(&dir.path().join(DB_FILENAME)).unwrap();
    let mut st = state_with(&[("u1", Status::Quarantined), ("u2", Status::Delivered)]);
    {
        let u1 = st.units.get_mut("u1").unwrap();
        u1.quarantine = Some(String::from("q1-abc"));
        u1.repairs = Some(String::from("q0-xyz"));
        u1.base = Some(String::from("pump/quarantine/q1-abc"));
        u1.extra
            .insert(String::from("future"), serde_json::json!(7));
    }
    st.units.get_mut("u2").unwrap().solo = true;
    store.save(&st).unwrap();
    let back = store.load().unwrap();
    let u1 = &back.units["u1"];
    assert_eq!(u1.status, Status::Quarantined);
    assert_eq!(u1.quarantine.as_deref(), Some("q1-abc"));
    assert_eq!(u1.repairs.as_deref(), Some("q0-xyz"));
    assert_eq!(u1.base.as_deref(), Some("pump/quarantine/q1-abc"));
    assert!(!u1.solo);
    assert_eq!(u1.extra["future"], serde_json::json!(7));
    let u2 = &back.units["u2"];
    assert!(u2.solo);
    assert_eq!(u2.quarantine, None);
}

#[test]
fn a_repair_that_met_another_batch_is_released_as_solo() {
    let (_dir, ctx, _host) = setup();
    let (mut st, id) = parked_pair(&ctx);
    let repair = repair_uid(&ctx, &id);
    let extra = state_with(&[("u3", Status::Merging)]);
    st.order.push(String::from("u3"));
    st.units
        .insert(String::from("u3"), extra.units["u3"].clone());
    st.units.get_mut(&repair).unwrap().status = Status::Merging;
    let q2 = Quarantine {
        id: String::from("q2-aaaaaa"),
        cmd: String::from("cargo test --lib"),
        code: 1,
        text: String::from("boom"),
        units: vec![repair.clone(), String::from("u3")],
    };
    for uid in [repair.as_str(), "u3"] {
        set_aside(&ctx, &mut st, &quarantined(uid, &q2));
    }
    let unit = &st.units[&repair];
    assert_eq!(unit.status, Status::Delivered);
    assert!(unit.solo);
    assert_eq!(unit.quarantine, None);
    assert_eq!(repair_of(&ctx, &st, &repair).as_deref(), Some(id.as_str()));
    assert_eq!(st.units["u3"].status, Status::Quarantined);
    assert_eq!(st.units["u3"].quarantine.as_deref(), Some("q2-aaaaaa"));
}
