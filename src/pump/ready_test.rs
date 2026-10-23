//! Tests for [`super::ready`].

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::ready::{blocked_needs, dependency_holds, satisfied};
use crate::pump::state::{NeedRecord, State, Status, Unit};

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

#[test]
fn an_unmerged_dependency_does_not_satisfy_and_a_merged_one_does() {
    let (_dir, _ctx, _host, mut st) = setup();
    let (dep_id, dep_u) = mk("dep", Status::Inflight, &["src/dep"]);
    st.units.insert(dep_id, dep_u);
    assert!(!satisfied(&st, "dep", 0));
    st.units.get_mut("dep").unwrap().status = Status::Merged;
    assert!(satisfied(&st, "dep", 0));
}

#[test]
fn a_no_defect_dependency_counts_as_satisfied() {
    let (_dir, _ctx, _host, mut st) = setup();
    let (dep_id, dep_u) = mk("dep", Status::NoDefect, &["src/dep"]);
    st.units.insert(dep_id, dep_u);
    assert!(satisfied(&st, "dep", 0));
}

#[test]
fn a_replaced_dependency_is_satisfied_only_when_every_replacement_is() {
    let (_dir, _ctx, _host, mut st) = setup();
    let (dep_id, mut dep) = mk("dep", Status::Replaced, &["src/dep"]);
    dep.replaced_by = vec!["r1".to_string(), "r2".to_string()];
    st.units.insert(dep_id, dep);
    let (r1_id, r1_u) = mk("r1", Status::Merged, &["src/dep"]);
    let (r2_id, r2_u) = mk("r2", Status::Pending, &["src/dep"]);
    st.units.insert(r1_id, r1_u);
    st.units.insert(r2_id, r2_u);
    assert!(!satisfied(&st, "dep", 0));
    st.units.get_mut("r2").unwrap().status = Status::Merged;
    assert!(satisfied(&st, "dep", 0));
}

#[test]
fn a_need_dependency_hold_names_the_dependency_until_it_lands() {
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
}

#[test]
fn an_unreadable_dependency_gate_holds_nothing_and_logs_it() {
    let (_dir, ctx, host, mut st) = setup();
    host.reply("graph blocked", 1, "error: no such graph");
    let (id, u) = mk("n-1", Status::Pending, &["src/u"]);
    st.units.insert(id, u);

    assert!(dependency_holds(&ctx, &st).is_empty());
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("graph blocked unreadable"))
    );
}

#[test]
fn blocked_needs_maps_needs_to_raw_prerequisites() {
    let (_dir, ctx, host, _st) = setup();
    host.reply(
        "graph blocked",
        0,
        r#"[{"source_id":"n","blocked_by":["a","b"]},{"source_id":"m","blocked_by":[]},{"source_id":"","blocked_by":["x"]}]"#,
    );
    let raw = blocked_needs(&ctx).expect("readable graph blocked output");
    let mut want = BTreeMap::new();
    want.insert("n".to_string(), vec!["a".to_string(), "b".to_string()]);
    assert_eq!(raw, want);

    host.reply("graph blocked", 1, "error: locked");
    assert!(blocked_needs(&ctx).is_none());
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("graph blocked unreadable"))
    );
}
