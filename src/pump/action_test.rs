//! Tests for [`crate::pump::action`]: operator actions posted on pump
//! records are applied once, close their record, and a new record revives a
//! need the pump had given up on. They run against a real mailspace, so the
//! records and the actions are the ones `vivi pump` shows.

use std::fs;

use super::*;
use crate::mailspace::{PumpActionRequest, PumpRecord, PumpState};
use crate::pump::fixture::{Project, T0, planned, project};
use crate::pump::state::Unit;
use crate::pump::store::{DB_FILENAME, PumpStore};

/// The open record of a project's need.
fn open_record(project: &Project, need: &str) -> PumpRecord {
    project.mailspace.pump_send(None, need, None).unwrap()
}

fn post(project: &Project, need: &str, request: &PumpActionRequest) -> String {
    project
        .mailspace
        .pump_post_action(None, need, request)
        .unwrap()
        .id
}

fn history(project: &Project, need: &str) -> Vec<PumpRecord> {
    project.mailspace.pump_history(need).unwrap()
}

/// The state as the pump last saved it.
fn saved(project: &Project) -> State {
    PumpStore::open_read_only(&project.home().join(DB_FILENAME))
        .unwrap()
        .unwrap()
        .load()
        .unwrap()
}

fn logged(project: &Project, text: &str) -> bool {
    project.host.logs().iter().any(|line| line.contains(text))
}

fn stuck_unit(need: &str, attempts: u32) -> Unit {
    Unit {
        need: need.to_owned(),
        target: String::from("default"),
        status: Status::Stuck,
        attempts,
        ..Unit::default()
    }
}

#[test]
fn a_requeue_action_is_applied_exactly_once_even_across_a_rerun_of_the_tick() {
    let project = project();
    project.host.reply("ls-remote", 0, "");
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    open_record(&project, &need);
    let mut st = planned(&need, &[("u1", Status::Stuck)]);
    st.units.get_mut("u1").unwrap().attempts = 1;
    let id = post(
        &project,
        &need,
        &PumpActionRequest::requeue("u1", None, false, false, "operator asked"),
    );

    apply_pending(&ctx, &mut st);

    assert_eq!(st.units["u1"].status, Status::Pending);
    assert!(logged(&project, "requeue u1 by operator: operator asked"));
    assert_eq!(st.board.actions[&id].kind, "requeue");

    // The same tick again changes nothing: the unit stays where it later got.
    st.units.get_mut("u1").unwrap().status = Status::Stuck;
    let before = project.host.logs().len();
    apply_pending(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Stuck, "applied twice");
    assert_eq!(project.host.logs().len(), before);

    // And so does a pump that restarts from its saved state: the applied
    // mark was saved with the state change.
    let mut reloaded = saved(&project);
    assert_eq!(reloaded.units["u1"].status, Status::Pending);
    assert!(reloaded.board.actions.contains_key(&id));
    reloaded.units.get_mut("u1").unwrap().status = Status::Stuck;
    apply_pending(&ctx, &mut reloaded);
    assert_eq!(reloaded.units["u1"].status, Status::Stuck, "replayed");
}

#[test]
fn a_requeue_of_a_unit_that_belongs_to_another_need_is_refused_and_consumed() {
    let project = project();
    project.host.reply("ls-remote", 0, "");
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    let other = project.need("fix billing");
    open_record(&project, &need);
    let mut st = planned(&other, &[("u9", Status::Stuck)]);
    let id = post(
        &project,
        &need,
        &PumpActionRequest::requeue("u9", None, false, false, "wrong need"),
    );

    apply_pending(&ctx, &mut st);

    assert_eq!(st.units["u9"].status, Status::Stuck);
    assert!(logged(&project, "requeue u9 refused: it belongs to need"));
    assert!(st.board.actions.contains_key(&id), "consumed, not retried");
}

#[test]
fn an_applied_withdraw_closes_the_record_withdrawn_once() {
    let project = project();
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    let record = open_record(&project, &need);
    let mut st = planned(&need, &[("u1", Status::Pending), ("u2", Status::Merged)]);
    let id = post(
        &project,
        &need,
        &PumpActionRequest::withdraw("operator changed their mind"),
    );

    apply_pending(&ctx, &mut st);

    assert_eq!(st.units["u1"].status, Status::Withdrawn);
    assert_eq!(st.units["u2"].status, Status::Merged);
    assert_eq!(st.closed_needs, vec![need.clone()]);
    let records = history(&project, &need);
    assert_eq!(records.len(), 1, "a withdraw opens no new record");
    assert_eq!(records[0].handle, record.handle);
    assert_eq!(records[0].status, PumpState::Done);
    assert_eq!(records[0].outcome.as_deref(), Some("withdrawn"));
    assert!(st.board.actions[&id].owes.is_empty(), "the close was paid");

    let before = project.host.logs().len();
    apply_pending(&ctx, &mut st);
    assert_eq!(project.host.logs().len(), before, "applied again");
}

#[test]
fn an_applied_replan_supersedes_the_record_opens_a_fresh_one_and_clears_the_planner_failures() {
    let project = project();
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    let first = open_record(&project, &need);
    let mut st = planned(&need, &[("u1", Status::Pending), ("u2", Status::Merged)]);
    st.board.planner_fails.insert(need.clone(), 3);
    st.plans_seen.push(format!("{need}.json"));
    let id = post(&project, &need, &PumpActionRequest::replan(false));

    apply_pending(&ctx, &mut st);

    let records = history(&project, &need);
    assert_eq!(records.len(), 2, "{records:?}");
    assert_eq!(records[0].handle, first.handle);
    assert_eq!(records[0].outcome.as_deref(), Some("superseded"));
    assert_eq!(records[1].status, PumpState::Open, "the replan is intake");
    assert!(!st.needs.contains_key(&need), "the plan is thrown away");
    assert!(st.board.planner_fails.is_empty());
    assert_eq!(st.units["u1"].status, Status::Withdrawn);
    assert_eq!(st.units["u2"].status, Status::Merged);
    assert!(st.board.actions[&id].owes.is_empty());

    // Replayed, it opens no third record.
    apply_pending(&ctx, &mut st);
    assert_eq!(history(&project, &need).len(), 2);
}

#[test]
fn a_refused_replan_leaves_the_record_alone_and_is_consumed() {
    let project = project();
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    open_record(&project, &need);
    let mut st = planned(&need, &[("u1", Status::Inflight)]);
    let id = post(&project, &need, &PumpActionRequest::replan(false));

    apply_pending(&ctx, &mut st);
    apply_pending(&ctx, &mut st);

    assert!(st.needs.contains_key(&need), "live work refuses");
    let records = history(&project, &need);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, PumpState::Open);
    assert!(st.board.actions.contains_key(&id));
    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert_eq!(
        exceptions.matches("replan-refused").count(),
        1,
        "refused once, not every tick: {exceptions}"
    );
}

#[test]
fn a_forced_replan_goes_over_live_work() {
    let project = project();
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    open_record(&project, &need);
    let mut st = planned(&need, &[("u1", Status::Inflight)]);
    post(&project, &need, &PumpActionRequest::replan(true));

    apply_pending(&ctx, &mut st);

    assert!(!st.needs.contains_key(&need));
    assert_eq!(st.units["u1"].status, Status::Inflight);
    assert_eq!(history(&project, &need).len(), 2);
}

#[test]
fn an_action_on_a_closed_record_is_never_applied() {
    let project = project();
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    let record = open_record(&project, &need);
    let mut st = planned(&need, &[("u1", Status::Pending)]);
    post(&project, &need, &PumpActionRequest::withdraw("too late"));
    project
        .mailspace
        .pump_done(&record.handle, Some("landed"))
        .unwrap();

    apply_pending(&ctx, &mut st);

    assert_eq!(st.units["u1"].status, Status::Pending);
    assert!(st.board.actions.is_empty());
}

#[test]
fn record_changes_wait_while_the_state_cannot_be_saved() {
    let project = project();
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    let record = open_record(&project, &need);
    let mut st = planned(&need, &[("u1", Status::Pending)]);
    let id = post(&project, &need, &PumpActionRequest::withdraw("stop"));
    // A directory where the database must be: the save fails.
    fs::create_dir_all(ctx.home.join(DB_FILENAME)).unwrap();

    apply_pending(&ctx, &mut st);

    let open = project.mailspace.pump_open_record(&need).unwrap();
    assert_eq!(open.map(|open| open.handle), Some(record.handle.clone()));
    assert_eq!(st.board.actions[&id].owes, "withdrawn", "still owed");

    fs::remove_dir_all(ctx.home.join(DB_FILENAME)).unwrap();
    apply_pending(&ctx, &mut st);

    assert!(project.mailspace.pump_open_record(&need).unwrap().is_none());
    assert!(st.board.actions[&id].owes.is_empty());
}

/// A saved mark that still owes `record` its `superseded` close, as a tick
/// that died after saving would leave it.
fn owing(st: &mut State, need: &str, record: &str) {
    st.board.actions.insert(
        String::from("replan-letter"),
        ActionMark {
            record: record.to_owned(),
            need: need.to_owned(),
            kind: String::from("replan"),
            owes: String::from("superseded"),
            detail: String::from("operator replan"),
        },
    );
}

#[test]
fn a_replan_that_died_before_closing_the_record_resumes_and_opens_one_fresh_record() {
    let project = project();
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    let first = open_record(&project, &need);
    let mut st = State::default();
    owing(&mut st, &need, &first.handle);

    apply_pending(&ctx, &mut st);
    apply_pending(&ctx, &mut st);

    let records = history(&project, &need);
    assert_eq!(records.len(), 2, "{records:?}");
    assert_eq!(records[0].outcome.as_deref(), Some("superseded"));
    assert_eq!(records[1].status, PumpState::Open);
    assert!(st.board.actions["replan-letter"].owes.is_empty());
}

#[test]
fn a_replan_that_died_between_the_close_and_the_open_resumes_with_the_open() {
    let project = project();
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    let first = open_record(&project, &need);
    project
        .mailspace
        .pump_done(&first.handle, Some("superseded"))
        .unwrap();
    let mut st = State::default();
    owing(&mut st, &need, &first.handle);

    apply_pending(&ctx, &mut st);

    let records = history(&project, &need);
    assert_eq!(records.len(), 2, "{records:?}");
    assert_eq!(records[1].status, PumpState::Open);
}

#[test]
fn a_replan_that_died_after_the_open_does_not_open_a_third_record() {
    let project = project();
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    let first = open_record(&project, &need);
    project
        .mailspace
        .pump_done(&first.handle, Some("superseded"))
        .unwrap();
    project.mailspace.pump_send(None, &need, None).unwrap();
    let mut st = State::default();
    owing(&mut st, &need, &first.handle);

    apply_pending(&ctx, &mut st);

    assert_eq!(history(&project, &need).len(), 2);
    assert!(st.board.actions["replan-letter"].owes.is_empty());
}

#[test]
fn a_new_record_for_a_given_up_need_requeues_its_stuck_units_instead_of_closing_gave_up_at_once() {
    let project = project();
    project.host.reply("ls-remote", 0, "");
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    let first = open_record(&project, &need);
    let mut st = planned(&need, &[("u0", Status::Merged), ("u1", Status::Stuck)]);
    st.units.get_mut("u1").unwrap().attempts = 3;
    apply_pending(&ctx, &mut st);
    assert_eq!(st.board.records[&need], first.handle);

    // The pump gives up on the dispatch.
    crate::pump::record::settle_gave_up(&ctx, &st, std::slice::from_ref(&need));
    assert_eq!(
        history(&project, &need)[0].outcome.as_deref(),
        Some("gave-up")
    );

    // The operator retries with a new record.
    let second = open_record(&project, &need);
    assert!(
        crate::pump::record::gave_up_reason(&ctx, &st, &need).is_some(),
        "the old state would give up at once"
    );
    // A board pass working from the old state leaves the new record alone.
    crate::pump::record::settle_gave_up(&ctx, &st, std::slice::from_ref(&need));
    assert_eq!(
        project
            .mailspace
            .pump_open_record(&need)
            .unwrap()
            .map(|r| r.handle),
        Some(second.handle.clone())
    );

    apply_pending(&ctx, &mut st);

    assert_eq!(st.board.records[&need], second.handle);
    assert_eq!(st.units["u1"].status, Status::Pending);
    assert_eq!(st.units["u1"].attempts, 0, "a fresh attempt budget");
    assert_eq!(st.units["u0"].status, Status::Merged);
    assert_eq!(crate::pump::record::gave_up_reason(&ctx, &st, &need), None);
    crate::pump::record::settle_gave_up(&ctx, &st, std::slice::from_ref(&need));
    assert_eq!(
        project
            .mailspace
            .pump_open_record(&need)
            .unwrap()
            .map(|r| r.handle),
        Some(second.handle)
    );
}

#[test]
fn a_record_the_pump_sees_for_the_first_time_disturbs_nothing() {
    let project = project();
    project.host.reply("ls-remote", 0, "");
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    open_record(&project, &need);
    let mut st = planned(&need, &[("u1", Status::Stuck)]);
    st.units.insert(String::from("u1"), stuck_unit(&need, 3));

    apply_pending(&ctx, &mut st);

    assert_eq!(st.units["u1"].status, Status::Stuck);
    assert_eq!(st.units["u1"].attempts, 3);
}

#[test]
fn a_tick_applies_a_posted_action_and_saves_it() {
    let project = project();
    project.host.reply("ls-remote", 0, "");
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    open_record(&project, &need);
    let mut st = planned(&need, &[("u1", Status::Stuck)]);
    st.units.get_mut("u1").unwrap().task = Some(String::from("task-1"));
    let mut store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    store.save(&st).unwrap();
    drop(store);
    let id = post(
        &project,
        &need,
        &PumpActionRequest::requeue("u1", None, false, false, "operator asked"),
    );
    project.host.set_now(T0);

    let (mut pass, requests, results) = crate::pump::boardpass::BoardPass::detached();
    crate::pump::run::tick(&ctx, &mut st, &mut pass).unwrap();
    pass.serve(&ctx, &mut st, &requests, &results);

    assert_eq!(
        st.units["u1"].status,
        Status::Pending,
        "the tick did not requeue u1"
    );
    assert_eq!(st.units["u1"].task, None);
    assert!(saved(&project).board.actions.contains_key(&id));
}

#[test]
fn an_unlock_action_is_neither_applied_nor_marked_by_the_old_pump() {
    let project = project();
    project.host.reply("ls-remote", 0, "");
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    open_record(&project, &need);
    let mut st = planned(&need, &[("u1", Status::Stuck)]);
    let id = post(
        &project,
        &need,
        &PumpActionRequest::unlock(Some("slice"), ""),
    );
    project.host.set_now(T0);

    let (mut pass, requests, results) = crate::pump::boardpass::BoardPass::detached();
    crate::pump::run::tick(&ctx, &mut st, &mut pass).unwrap();
    pass.serve(&ctx, &mut st, &requests, &results);

    assert!(
        !st.board.actions.contains_key(&id),
        "the old pump marked an unlock"
    );
    assert!(!saved(&project).board.actions.contains_key(&id));
    assert_eq!(st.units["u1"].status, Status::Stuck);
}

#[test]
fn legacy_open_records_are_not_new_dispatches_of_need_empty() {
    let project = project();
    let ctx = project.pump_ctx();
    let legacy = |handle: &str| PumpRecord {
        handle: handle.to_owned(),
        kind: String::from("pump"),
        need: String::new(),
        status: PumpState::Open,
        from: String::new(),
        to: String::new(),
        date: String::new(),
        subject: String::new(),
        outcome: None,
        letter_error: Some(format!("pump record {handle}: old format")),
    };
    let mut st = State::default();
    st.board.planner_fails.insert(String::new(), 3);

    revive_new_records(&ctx, &mut st, &[legacy("aaa"), legacy("bbb")]);
    revive_new_records(&ctx, &mut st, &[legacy("bbb"), legacy("aaa")]);

    assert!(!logged(&project, "is a new dispatch of need"));
    assert!(st.board.records.is_empty(), "{:?}", st.board.records);
    assert_eq!(st.board.planner_fails.get(""), Some(&3));
}
