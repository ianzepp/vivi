//! Snapshot building and role gating for the boot pump section.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use tempfile::tempdir;

use super::gate::PumpView;
use super::{PumpSources, build_snapshot, default_view};
use crate::boot::pump_slots::{self, LocalSlotMark};
use crate::pool::config::PoolConfig;
use crate::pump::report::{self, PumpRead};
use crate::pump::state::{GateRun, NeedRecord, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::{RoleRouting, RoutedPool, RoutedRole};

fn role(name: &str, kind: &str, pool: Option<&str>) -> RoutedRole {
    RoutedRole {
        name: name.to_string(),
        kind: Some(kind.to_string()),
        status: String::from("active"),
        pool: pool.map(str::to_string),
        read_only: None,
        model: None,
        models: Vec::new(),
    }
}

fn routing() -> RoleRouting {
    RoleRouting {
        roles: vec![
            role("cto", "head", None),
            role("hand", "hand", Some("lanes")),
            role("merge", "merge", Some("lanes")),
        ],
        pools: BTreeMap::from([(
            String::from("lanes"),
            RoutedPool {
                slots: 2,
                provider: String::from("local"),
                fallback: None,
                caps: BTreeMap::new(),
                models: Vec::new(),
                has_bands: false,
            },
        )]),
    }
}

fn pool_toml(provider: &str, extra: &str) -> PoolConfig {
    toml::from_str(&format!("provider = \"{provider}\"\nslots = 2\n{extra}")).expect("pool toml")
}

fn unit(need: &str, status: Status) -> Unit {
    Unit {
        need: need.to_string(),
        status,
        since: 100,
        since_dispatch: Some(400),
        pool: Some(String::from("lanes")),
        task: Some(String::from("taskhand1")),
        branch: Some(String::from("unit/u1-a1")),
        model: Some(String::from("glm-4.6")),
        after: Vec::new(),
        ..Unit::default()
    }
}

fn state() -> State {
    let mut st = State::default();
    st.needs.insert(
        String::from("need1"),
        NeedRecord {
            verdict: String::from("units"),
            units: vec![
                String::from("u-inflight"),
                String::from("u-delivered"),
                String::from("u-held"),
                String::from("u-stuck"),
            ],
            revision: 1,
            ..NeedRecord::default()
        },
    );
    st.units
        .insert(String::from("u-inflight"), unit("need1", Status::Inflight));
    let mut delivered = unit("need1", Status::Delivered);
    delivered.task = Some(String::from("taskdeliv"));
    delivered.branch = Some(String::from("unit/u2-a1"));
    delivered.since_dispatch = Some(100);
    st.units.insert(String::from("u-delivered"), delivered);
    let mut held = unit("need1", Status::Held);
    held.task = None;
    held.held_on = Some(String::from("u-delivered"));
    held.after = vec![String::from("u-delivered")];
    held.branch = None;
    st.units.insert(String::from("u-held"), held);
    let mut stuck = unit("need1", Status::Stuck);
    stuck.task = Some(String::from("taskstuck"));
    st.units.insert(String::from("u-stuck"), stuck);
    st.order = vec![
        String::from("u-inflight"),
        String::from("u-delivered"),
        String::from("u-held"),
        String::from("u-stuck"),
    ];
    st.last_gate = GateRun {
        at: 900,
        task: Some(String::from("g1")),
        pool: Some(String::from("lanes")),
        ..GateRun::default()
    };
    st.closed_needs = vec![String::from("need-ready")];
    st
}

fn slot(root: &std::path::Path) -> LocalSlotMark {
    LocalSlotMark {
        pool: String::from("lanes"),
        id: String::from("demo/lanes/slot-1"),
        dir: root.join("demo/lanes/slot-1"),
        checkout: Some(root.join("demo/lanes/slot-1/repo")),
        task: Some(String::from("taskhand1")),
        claimed_at: Some(400),
        base_ref: Some(String::from("next")),
        base_sha: Some(String::from("abc1234")),
    }
}

#[allow(clippy::too_many_arguments)]
fn sources<'a>(
    routing: &'a RoleRouting,
    pools: &'a BTreeMap<String, PoolConfig>,
    state: &'a State,
    slots: &'a [LocalSlotMark],
    holders: &'a BTreeMap<String, String>,
    open_needs: &'a BTreeSet<String>,
    exceptions: &'a [(u64, String, String, String)],
    status: Option<&'a str>,
    complete: BTreeSet<String>,
) -> PumpSources<'a> {
    PumpSources {
        now: 1000,
        budget_secs: 600,
        routing,
        pools,
        state: Some(state),
        exception_rows: exceptions,
        status_txt: status,
        slots,
        holders,
        open_needs,
        complete_needs: complete,
        pump_to: "hand",
    }
}

#[test]
fn coordinating_roles_get_the_full_view() {
    let routing = routing();
    assert_eq!(default_view(routing.lookup("cto"), false), PumpView::Full);
    assert_eq!(default_view(routing.lookup("merge"), false), PumpView::Full);
    assert_eq!(default_view(None, false), PumpView::Full);
}

#[test]
fn a_hand_with_a_pool_gets_own_and_a_slot_without_a_role_does_too() {
    let routing = routing();
    assert_eq!(default_view(routing.lookup("hand"), false), PumpView::Own);
    assert_eq!(default_view(None, true), PumpView::Own);
}

#[test]
fn full_snapshot_lists_inflight_queue_held_stuck_and_pools() {
    let dir = tempdir().unwrap();
    let routing = routing();
    let pools = BTreeMap::from([
        (
            String::from("lanes"),
            pool_toml(
                "local",
                &format!(
                    "build_dir = \"build\"\n[local]\nroot = \"{}\"\nworktree_of = \"/tmp/proj\"\n",
                    dir.path().display()
                ),
            ),
        ),
        (String::from("fly4"), pool_toml("fly", "")),
    ]);
    let state = state();
    let marks = [slot(dir.path())];
    let holders = BTreeMap::from([(String::from("taskhand1"), String::from("hand"))]);
    let open = BTreeSet::from([String::from("need1")]);
    let exceptions = [(
        50_u64,
        String::from("stuck"),
        String::from("u-stuck"),
        String::from("needs a person"),
    )];
    let status =
        "last promote 2h ago (deadbeef)\nlane default/0 idle\nlane default/1 busy 1 units 3m\n";
    let src = sources(
        &routing,
        &pools,
        &state,
        &marks,
        &holders,
        &open,
        &exceptions,
        Some(status),
        BTreeSet::from([String::from("need1")]),
    );
    let snap = build_snapshot(&src, PumpView::Full, Some("cto"), None);
    assert_eq!(snap.inflight.len(), 1);
    assert_eq!(snap.inflight[0].unit, "u-inflight");
    assert_eq!(snap.inflight[0].role.as_deref(), Some("hand"));
    assert_eq!(snap.inflight[0].slot.as_deref(), Some("demo/lanes/slot-1"));
    assert!(
        snap.inflight[0]
            .checkout
            .as_ref()
            .is_some_and(|p| p.ends_with("slot-1/repo")),
        "{:?}",
        snap.inflight[0].checkout
    );
    assert_eq!(snap.inflight[0].base_ref.as_deref(), Some("next"));
    assert_eq!(snap.merge_queue.len(), 1);
    assert_eq!(snap.merge_queue[0].unit, "u-delivered");
    assert_eq!(snap.held.len(), 1);
    assert_eq!(snap.held[0].held_on.as_deref(), Some("u-delivered"));
    assert_eq!(snap.stuck.len(), 1);
    assert_eq!(snap.exceptions.len(), 1);
    assert_eq!(snap.lanes.len(), 2);
    assert_eq!(snap.last_promote.as_deref(), Some("2h ago (deadbeef)"));
    assert_eq!(
        snap.gate_pending.as_ref().unwrap().task.as_deref(),
        Some("g1")
    );
    assert!(snap.needs_ready.contains(&String::from("need1")));
    assert_eq!(snap.pools.len(), 2);
    let lanes = snap.pools.iter().find(|p| p.name == "lanes").unwrap();
    assert!(!lanes.remote);
    assert_eq!(lanes.used, 1);
    assert_eq!(lanes.roles, ["hand", "merge"]);
    let fly = snap.pools.iter().find(|p| p.name == "fly4").unwrap();
    assert!(fly.remote);
}

#[test]
fn own_view_keeps_only_the_hands_units() {
    let dir = tempdir().unwrap();
    let routing = routing();
    let pools = BTreeMap::from([(String::from("lanes"), pool_toml("local", ""))]);
    let state = state();
    let marks = [slot(dir.path())];
    let holders = BTreeMap::from([(String::from("taskhand1"), String::from("hand"))]);
    let open = BTreeSet::new();
    let src = sources(
        &routing,
        &pools,
        &state,
        &marks,
        &holders,
        &open,
        &[],
        None,
        BTreeSet::new(),
    );
    let snap = build_snapshot(&src, PumpView::Own, Some("hand"), Some(&marks[0]));
    assert_eq!(snap.inflight.len(), 1);
    assert_eq!(snap.inflight[0].unit, "u-inflight");
    assert!(snap.merge_queue.is_empty());
    assert!(snap.held.is_empty());
    assert!(snap.pools.is_empty());
    assert!(snap.lanes.is_empty());
    assert!(snap.exceptions.is_empty());
}

#[test]
fn none_is_not_built_by_the_snapshot_helper_callers() {
    // `--pump none` skips `build_snapshot` entirely; the enum still round-trips.
    assert_eq!(PumpView::None.as_str(), "none");
}

#[test]
fn a_fixture_pump_db_and_status_txt_feed_the_builder() {
    let dir = tempdir().unwrap();
    let home = dir.path().join("pump");
    fs::create_dir_all(&home).unwrap();
    let mut store = PumpStore::open(&home.join(DB_FILENAME)).unwrap();
    store.save(&state()).unwrap();
    crate::pump::store::append_exception(&home, 40, "stuck", "u-stuck", "parked");
    fs::write(
        home.join("status.txt"),
        "last promote never\nlane default/0 idle\n",
    )
    .unwrap();
    let PumpRead::Ready(loaded) = report::read(&home).unwrap() else {
        panic!("expected a readable pump.db");
    };
    let routing = routing();
    let pools = BTreeMap::from([(String::from("lanes"), pool_toml("local", ""))]);
    let holders = BTreeMap::new();
    let open = BTreeSet::new();
    let slots: [LocalSlotMark; 0] = [];
    let status = fs::read_to_string(home.join("status.txt")).unwrap();
    let src = PumpSources {
        now: 1000,
        budget_secs: 600,
        routing: &routing,
        pools: &pools,
        state: Some(loaded.state()),
        exception_rows: loaded.exception_rows(),
        status_txt: Some(&status),
        slots: &slots,
        holders: &holders,
        open_needs: &open,
        complete_needs: loaded.complete_needs(),
        pump_to: "hand",
    };
    let snap = build_snapshot(&src, PumpView::Full, None, None);
    assert_eq!(snap.inflight.len(), 1);
    assert_eq!(snap.last_promote.as_deref(), Some("never"));
    assert_eq!(snap.exceptions.len(), 1);
    assert_eq!(snap.exceptions[0].kind, "stuck");
}

#[test]
fn remote_units_without_a_local_slot_are_marked_remote() {
    let routing = routing();
    let pools = BTreeMap::from([(String::from("lanes"), pool_toml("fly", ""))]);
    let mut st = State::default();
    st.units
        .insert(String::from("u-inflight"), unit("need1", Status::Inflight));
    st.order = vec![String::from("u-inflight")];
    let holders = BTreeMap::new();
    let open = BTreeSet::new();
    let src = sources(
        &routing,
        &pools,
        &st,
        &[],
        &holders,
        &open,
        &[],
        None,
        BTreeSet::new(),
    );
    let snap = build_snapshot(&src, PumpView::Full, None, None);
    assert_eq!(snap.inflight[0].slot.as_deref(), Some("remote"));
}

#[test]
fn empty_sources_yield_an_empty_snapshot() {
    let routing = RoleRouting::default();
    let pools = BTreeMap::new();
    let holders = BTreeMap::new();
    let open = BTreeSet::new();
    let src = PumpSources {
        now: 0,
        budget_secs: 600,
        routing: &routing,
        pools: &pools,
        state: None,
        exception_rows: &[],
        status_txt: None,
        slots: &[],
        holders: &holders,
        open_needs: &open,
        complete_needs: BTreeSet::new(),
        pump_to: "hand",
    };
    let snap = build_snapshot(&src, PumpView::Full, None, None);
    assert!(snap.inflight.is_empty());
    assert!(snap.pools.is_empty());
}

#[test]
fn slot_detect_matches_cwd_under_the_slot_dir() {
    let dir = tempdir().unwrap();
    let slot_dir = dir.path().join("demo/lanes/slot-1");
    fs::create_dir_all(slot_dir.join("repo")).unwrap();
    let mark = LocalSlotMark {
        dir: slot_dir.clone(),
        checkout: Some(slot_dir.join("repo")),
        ..slot(dir.path())
    };
    let marks = [mark.clone()];
    let checkout = slot_dir.join("repo");
    let found = pump_slots::detect_here(&marks, Some(&checkout));
    assert_eq!(found.map(|s| s.id.as_str()), Some("demo/lanes/slot-1"));
}

/// A minimal local slot directory in the layout `vivi pool` writes.
fn write_slot(root: &std::path::Path, space: &str, index: u32) {
    let dir = root.join(space).join("here").join(format!("slot-{index}"));
    fs::create_dir_all(dir.join("marks")).unwrap();
    fs::create_dir_all(dir.join("repo")).unwrap();
    fs::write(dir.join("marks/vivi_pool"), "here\n").unwrap();
    fs::write(dir.join("marks/vivi_pool_mailspace"), format!("{space}\n")).unwrap();
    fs::write(dir.join("marks/vivi_pool_slot"), format!("{index}\n")).unwrap();
}

#[test]
fn scan_uses_the_id_qualified_shared_root() {
    const ID_A: &str = "12345678-1234-5678-1234-567812345678";
    const ID_B: &str = "99999999-9999-9999-9999-999999999999";
    let dir = tempdir().unwrap();
    let root = dir.path();
    write_slot(root, ID_A, 1);
    write_slot(root, ID_B, 1);
    write_slot(root, "app", 1);
    let text = format!(
        "name = \"app\"\n\
         id = \"{ID_A}\"\n\
         \n\
         [pools.here]\n\
         provider = \"local\"\n\
         slots = 1\n\
         \n\
         [pools.here.local]\n\
         root = {root:?}\n",
        root = root.to_string_lossy()
    );
    let config: crate::mailspace::MailspaceConfig = toml::from_str(&text).expect("mailspace toml");
    let mut ids: Vec<String> = pump_slots::scan(&config, root)
        .into_iter()
        .map(|mark| mark.id)
        .collect();
    ids.sort();
    assert_eq!(ids, vec![format!("{ID_A}/here/slot-1")]);
}
