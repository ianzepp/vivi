//! Tests for the old pump's hand-off of operator actions to the flow engine:
//! a need with an active run keeps its actions unapplied and unmarked; a need
//! without one is handled as before.

use super::*;
use crate::mailspace::PumpActionRequest;
use crate::pump::fixture::{Project, planned, project};
use crate::pump::flow::index::RunRow;

fn run_row(need: &str, state: &str) -> RunRow {
    RunRow {
        need: need.to_owned(),
        run: 1,
        flow: String::from("build"),
        flow_version: 2,
        kind: String::from("build"),
        entry: String::from("plan"),
        state: state.to_owned(),
    }
}

/// A need with an open record, one stuck unit, a requeue posted on it, and a
/// run row in `run_state` when given.
fn rig(run_state: Option<&str>) -> (Project, String, State, String) {
    let project = project();
    project.host.reply("ls-remote", 0, "");
    let need = project.need("fix auth");
    project.mailspace.pump_send(None, &need, None).unwrap();
    let mut st = planned(&need, &[("u1", Status::Stuck)]);
    st.units.get_mut("u1").unwrap().attempts = 1;
    let mut store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    store.save(&st).unwrap();
    if let Some(state) = run_state {
        store.insert_run(&run_row(&need, state)).unwrap();
    }
    let id = project
        .mailspace
        .pump_post_action(
            None,
            &need,
            &PumpActionRequest::requeue("u1", None, false, false, "operator asked"),
        )
        .unwrap()
        .id;
    (project, need, st, id)
}

#[test]
fn an_action_on_a_need_with_a_live_run_is_neither_applied_nor_marked() {
    let (project, _need, mut st, id) = rig(Some("live"));
    let ctx = project.pump_ctx();

    apply_pending(&ctx, &mut st);

    assert_eq!(
        st.units["u1"].status,
        Status::Stuck,
        "applied by the old path"
    );
    assert!(
        !st.board.actions.contains_key(&id),
        "marked by the old path"
    );
}

#[test]
fn an_action_on_a_need_with_a_finished_run_is_applied_as_before() {
    let (project, _need, mut st, id) = rig(Some("done"));
    let ctx = project.pump_ctx();

    apply_pending(&ctx, &mut st);

    assert_eq!(st.units["u1"].status, Status::Pending);
    assert!(st.board.actions.contains_key(&id));
}

#[test]
fn an_action_on_a_need_without_a_run_is_applied_as_before() {
    let (project, _need, mut st, id) = rig(None);
    let ctx = project.pump_ctx();

    apply_pending(&ctx, &mut st);

    assert_eq!(st.units["u1"].status, Status::Pending);
    assert!(st.board.actions.contains_key(&id));
}
