//! Tests for [`crate::pump::adopt`]: records opened for the work that was in
//! flight at the vivi 19 upgrade, and nothing else.

use super::*;
use crate::cli::PumpAdoptCommand;
use crate::mailspace::PumpState;
use crate::pump::fixture::{planned, project, send_need};
use crate::pump::state::Status;
use crate::pump::store::{DB_FILENAME, PumpStore};

fn state_of(needs: &[(&str, &[(&str, Status)])]) -> State {
    let mut st = State::default();
    for (need, units) in needs {
        let one = planned(need, units);
        st.needs.extend(one.needs);
        st.units.extend(one.units);
    }
    st
}

fn handles(pairs: &[(String, String)]) -> Vec<&str> {
    pairs.iter().map(|(need, _)| need.as_str()).collect()
}

#[test]
fn in_flight_needs_are_the_planned_and_planning_ones_that_are_not_finished() {
    let mut st = state_of(&[
        ("running", &[("r1", Status::Inflight)]),
        ("done", &[("d1", Status::Merged)]),
        ("withdrawn", &[("w1", Status::Pending)]),
        ("closed", &[("c1", Status::Pending)]),
        ("parked", &[]),
    ]);
    st.needs.get_mut("withdrawn").unwrap().withdrawn = true;
    st.closed_needs.push(String::from("closed"));
    st.board.planning.insert(String::from("planning"), 1);
    st.board
        .revision_planning
        .insert(String::from("revising"), 1);

    assert_eq!(
        in_flight_needs(&st),
        vec!["parked", "planning", "revising", "running"]
    );
}

#[test]
fn adopt_opens_a_record_for_each_in_flight_need_and_is_idempotent() {
    let project = project();
    let running = project.need("running");
    let finished = project.need("finished");
    let st = state_of(&[
        (&running, &[("r1", Status::Inflight)]),
        (&finished, &[("f1", Status::Merged)]),
    ]);

    let first = adopt(&project.mailspace, &st, false).unwrap();

    assert_eq!(handles(&first.opened), vec![running.as_str()]);
    assert!(first.kept.is_empty() && first.skipped.is_empty());
    let record = project
        .mailspace
        .pump_open_record(&running)
        .unwrap()
        .unwrap();
    assert_eq!(record.status, PumpState::Open);
    assert_eq!(first.opened[0].1, record.handle);
    assert!(
        project
            .mailspace
            .pump_history(&finished)
            .unwrap()
            .is_empty()
    );

    let second = adopt(&project.mailspace, &st, false).unwrap();

    assert!(second.opened.is_empty(), "{second:?}");
    assert_eq!(handles(&second.kept), vec![running.as_str()]);
    assert_eq!(project.mailspace.pump_history(&running).unwrap().len(), 1);
}

#[test]
fn adopt_leaves_a_need_that_already_has_an_open_record_alone() {
    let project = project();
    let need = project.need("running");
    let record = project.mailspace.pump_send(None, &need, None).unwrap();
    let st = state_of(&[(&need, &[("r1", Status::Pending)])]);

    let report = adopt(&project.mailspace, &st, false).unwrap();

    assert!(report.opened.is_empty());
    assert_eq!(report.kept, vec![(need.clone(), record.handle)]);
}

#[test]
fn adopt_reports_what_it_cannot_adopt_instead_of_failing() {
    let project = project();
    let stray = send_need(&project.mailspace, "not for the pump", "elsewhere");
    let st = state_of(&[
        ("pumpfix-g1", &[("p1", Status::Pending)]),
        (&stray, &[("s1", Status::Pending)]),
    ]);

    let report = adopt(&project.mailspace, &st, false).unwrap();

    assert!(report.opened.is_empty());
    let skipped: Vec<&str> = report.skipped.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(skipped.len(), 2, "{report:?}");
    assert!(skipped.contains(&"pumpfix-g1") && skipped.contains(&stray.as_str()));
    let why = report
        .skipped
        .iter()
        .find(|(need, _)| need == &stray)
        .map(|(_, why)| why.as_str())
        .unwrap();
    assert!(why.contains("not addressed to the pump identity"), "{why}");
}

#[test]
fn a_dry_run_opens_nothing() {
    let project = project();
    let need = project.need("running");
    let st = state_of(&[(&need, &[("r1", Status::Pending)])]);

    let report = adopt(&project.mailspace, &st, true).unwrap();

    assert_eq!(handles(&report.opened), vec![need.as_str()]);
    assert!(project.mailspace.pump_history(&need).unwrap().is_empty());
}

#[test]
fn run_reads_the_saved_state_and_adopts_it() {
    let project = project();
    let need = project.need("running");
    let st = state_of(&[(&need, &[("r1", Status::Pending)])]);
    let mut store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    store.save(&st).unwrap();
    drop(store);
    let cmd = PumpAdoptCommand {
        dry_run: false,
        project: None,
    };

    run(project.root(), &cmd).unwrap();
    run(project.root(), &cmd).unwrap();

    assert_eq!(project.mailspace.pump_history(&need).unwrap().len(), 1);
}

#[test]
fn run_without_a_pump_database_is_refused() {
    let project = project();
    let cmd = PumpAdoptCommand {
        dry_run: false,
        project: None,
    };

    let error = run(project.root(), &cmd).unwrap_err();

    assert!(error.to_string().contains("no pump.db at"), "{error}");
}

#[test]
fn an_adopted_need_is_noted_not_revived_when_the_pump_first_sees_its_record() {
    let project = project();
    project.host.reply("ls-remote", 0, "");
    let need = project.need("running");
    let mut st = state_of(&[(&need, &[("r1", Status::Stuck)])]);
    st.units.get_mut("r1").unwrap().attempts = 3;
    adopt(&project.mailspace, &st, false).unwrap();

    crate::pump::action::apply_pending(&project.pump_ctx(), &mut st);

    assert_eq!(st.units["r1"].status, Status::Stuck);
    assert_eq!(st.units["r1"].attempts, 3);
    assert!(st.board.records.contains_key(&need));
}
