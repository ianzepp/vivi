//! Tests for leftover quarantine state: the stuck-repair release and the
//! status and store round trips of parked units.

use tempfile::{TempDir, tempdir};

use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::quarantine::{repair_of, repair_stuck, repair_uid};
use crate::pump::state::{NeedRecord, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore};

fn setup() -> (TempDir, Ctx, std::sync::Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
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

#[test]
fn quarantined_units_are_counted_and_named() {
    let st = state_with(&[("u1", Status::Quarantined), ("u2", Status::Delivered)]);
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
fn a_stuck_repair_returns_its_quarantined_units_to_delivered_solo() {
    let (_dir, ctx, _host) = setup();
    let mut st = state_with(&[("u1", Status::Quarantined), ("u2", Status::Quarantined)]);
    for uid in ["u1", "u2"] {
        st.units.get_mut(uid).unwrap().quarantine = Some(String::from("q1-abc"));
    }
    let repair = repair_uid(&ctx, "q1-abc");
    st.order.push(repair.clone());
    st.units.insert(
        repair.clone(),
        Unit {
            repairs: Some(String::from("q1-abc")),
            solo: true,
            status: Status::Inflight,
            ..Unit::default()
        },
    );
    assert_eq!(repair_of(&ctx, &st, &repair).as_deref(), Some("q1-abc"));
    repair_stuck(&ctx, &mut st, &repair);
    for uid in ["u1", "u2"] {
        assert_eq!(st.units[uid].status, Status::Delivered);
        assert!(st.units[uid].solo);
        assert_eq!(st.units[uid].quarantine, None);
    }
    repair_stuck(&ctx, &mut st, "u1");
    assert_eq!(st.units["u1"].status, Status::Delivered);
}
