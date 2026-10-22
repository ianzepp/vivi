//! Tests for merge-job selection: batching, held-target recovery, and
//! closed-need exclusion.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::merge::select::{eligible, recheck_held, take_batch};
use crate::pump::state::{State, Status, Unit};

fn delivered(need: &str, solo: bool) -> Unit {
    Unit {
        need: need.to_owned(),
        status: Status::Delivered,
        branch: Some(format!("unit/{need}")),
        solo,
        ..Unit::default()
    }
}

#[test]
fn take_batch_sends_a_solo_unit_alone_and_leaves_the_rest() {
    let mut st = State::default();
    st.units.insert(String::from("solo"), delivered("n1", true));
    st.units.insert(String::from("a"), delivered("n2", false));
    st.units.insert(String::from("b"), delivered("n3", false));
    let mut queue = VecDeque::from([String::from("solo"), String::from("a"), String::from("b")]);
    assert_eq!(take_batch(&st, &mut queue, 8), vec!["solo"]);
    assert_eq!(
        queue.into_iter().collect::<Vec<_>>(),
        vec![String::from("a"), String::from("b")]
    );
}

#[test]
fn take_batch_skips_a_later_solo_unit_until_the_next_job() {
    let mut st = State::default();
    st.units.insert(String::from("a"), delivered("n1", false));
    st.units.insert(String::from("solo"), delivered("n2", true));
    st.units.insert(String::from("b"), delivered("n3", false));
    let mut queue = VecDeque::from([String::from("a"), String::from("solo"), String::from("b")]);
    assert_eq!(take_batch(&st, &mut queue, 8), vec!["a", "b"]);
    assert_eq!(
        queue.into_iter().collect::<Vec<_>>(),
        vec![String::from("solo")]
    );
}

#[test]
fn recheck_held_returns_units_whose_target_is_configured() {
    let mut st = State::default();
    st.units.insert(
        String::from("held"),
        Unit {
            status: Status::Held,
            branch: Some(String::from("unit/held")),
            target: String::from("tela"),
            ..Unit::default()
        },
    );
    let mut targets = BTreeMap::new();
    targets.insert(String::from("tela"), 1);
    recheck_held(&mut st, &targets);
    assert_eq!(st.units["held"].status, Status::Delivered);
}

#[test]
fn a_held_merge_still_admits_units_of_pump_created_gate_fix_needs() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.reply("need list --status done", 0, "[]");
    std::fs::create_dir_all(&ctx.home).unwrap();
    std::fs::write(ctx.home.join("HOLD-MERGE"), "red\n").unwrap();
    let mut st = State::default();
    st.units
        .insert(String::from("plain-1"), delivered("plain", false));
    st.units
        .insert(String::from("8c4329ff-1"), delivered("8c4329ff", false));
    st.units
        .insert(String::from("pumpfix-q1"), delivered("q1", false));
    st.order = vec![
        String::from("plain-1"),
        String::from("8c4329ff-1"),
        String::from("pumpfix-q1"),
    ];
    st.red
        .insert(String::from("gate::x"), String::from("8c4329ff"));
    let known = BTreeSet::from([String::from("default")]);

    let waiting = eligible(&ctx, &mut st, &known);

    let default = waiting.get("default").cloned().unwrap_or_default();
    assert_eq!(
        default.into_iter().collect::<Vec<_>>(),
        vec!["8c4329ff-1", "pumpfix-q1"]
    );
}

#[test]
fn eligible_skips_units_whose_need_was_closed_on_the_board() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.reply(
        "need list --status done",
        0,
        r#"[{"handle":"closed-need","subject":"done"}]"#,
    );
    let mut st = State::default();
    st.units
        .insert(String::from("open"), delivered("open-need", false));
    st.units
        .insert(String::from("closed"), delivered("closed-need", false));
    st.order = vec![String::from("open"), String::from("closed")];
    let known = BTreeSet::from([String::from("default")]);
    let waiting = eligible(&ctx, &mut st, &known);
    let default = waiting.get("default").cloned().unwrap_or_default();
    assert_eq!(default.into_iter().collect::<Vec<_>>(), vec!["open"]);
}
