//! Tests for [`crate::pump::record`]: how a dispatch's pump record is closed
//! (`landed`, `gave-up`, `withdrawn`, `superseded`), driven against a real
//! mailspace so the records are the ones `vivi pump list` shows.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::*;
use crate::mailspace::{Mailspace, PumpRecord, SendRequest};
use crate::pump::config::PumpConfig;
use crate::pump::host::{FakeHost, test_ctx};
use crate::pump::state::{NeedRecord, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore};

const T0: u64 = 1_000_000;

/// A context whose project is a real mailspace with the `operator` and
/// `mind` identities, plus the mailspace and one open need.
struct World {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    mailspace: Mailspace,
    need: String,
}

fn world() -> World {
    let dir = tempdir().unwrap();
    let project = dir.path().join("project");
    let mut mailspace = Mailspace::init(Some(&project)).unwrap();
    mailspace.add_identity("operator").unwrap();
    mailspace.add_identity("mind").unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.set_now(T0);
    let need = send_need(&mailspace, "fix auth");
    World {
        _dir: dir,
        ctx,
        host,
        mailspace,
        need,
    }
}

fn send_need(mailspace: &Mailspace, subject: &str) -> String {
    mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: "done_when: it works".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

fn history(world: &World) -> Vec<PumpRecord> {
    world.mailspace.pump_history(&world.need).unwrap()
}

fn only_record(world: &World) -> PumpRecord {
    let mut records = history(world);
    assert_eq!(records.len(), 1, "{records:?}");
    records.remove(0)
}

fn unit(need: &str, status: Status) -> Unit {
    Unit {
        need: need.to_owned(),
        target: String::from("default"),
        status,
        ..Unit::default()
    }
}

/// State holding `need` planned into the given `(uid, status)` units.
fn planned(need: &str, units: &[(&str, Status)]) -> State {
    let mut st = State::default();
    st.needs.insert(
        need.to_owned(),
        NeedRecord {
            verdict: String::from("units"),
            units: units.iter().map(|(uid, _)| (*uid).to_owned()).collect(),
            ..NeedRecord::default()
        },
    );
    for (uid, status) in units {
        st.units.insert((*uid).to_owned(), unit(need, *status));
    }
    st
}

fn closed(need: &str) -> BTreeSet<String> {
    [need.to_owned()].into()
}

#[test]
fn a_closed_need_with_every_unit_merged_closes_its_record_landed() {
    let world = world();
    let record = world.mailspace.pump_send(None, &world.need, None).unwrap();
    let st = planned(
        &world.need,
        &[("u1", Status::Merged), ("u2", Status::NoDefect)],
    );

    settle_closed_needs(&world.ctx, &st, &closed(&world.need));

    let done = only_record(&world);
    assert_eq!(done.handle, record.handle);
    assert_eq!(done.status, PumpState::Done);
    assert_eq!(done.outcome.as_deref(), Some("landed"));
}

#[test]
fn a_need_closed_with_work_unfinished_closes_its_record_withdrawn() {
    let world = world();
    world.mailspace.pump_send(None, &world.need, None).unwrap();
    let st = planned(
        &world.need,
        &[("u1", Status::Merged), ("u2", Status::Inflight)],
    );

    settle_closed_needs(&world.ctx, &st, &closed(&world.need));

    assert_eq!(only_record(&world).outcome.as_deref(), Some("withdrawn"));
}

#[test]
fn a_need_closed_before_it_was_planned_closes_its_record_withdrawn() {
    let world = world();
    world.mailspace.pump_send(None, &world.need, None).unwrap();

    settle_closed_needs(&world.ctx, &State::default(), &closed(&world.need));

    assert_eq!(only_record(&world).outcome.as_deref(), Some("withdrawn"));
}

#[test]
fn an_open_need_keeps_its_record_open() {
    let world = world();
    world.mailspace.pump_send(None, &world.need, None).unwrap();
    let st = planned(&world.need, &[("u1", Status::Merged)]);

    settle_closed_needs(&world.ctx, &st, &closed("some-other-need"));

    assert_eq!(only_record(&world).status, PumpState::Open);
}

#[test]
fn finish_closes_only_the_open_record_and_a_second_call_is_a_no_op() {
    let world = world();
    let first = world.mailspace.pump_send(None, &world.need, None).unwrap();
    world
        .mailspace
        .pump_done(&first.handle, Some("gave-up"))
        .unwrap();
    assert_eq!(finish(&world.ctx, &world.need, WITHDRAWN, ""), None);
    let second = world.mailspace.pump_send(None, &world.need, None).unwrap();

    assert_eq!(
        finish(&world.ctx, &world.need, GAVE_UP, "").as_deref(),
        Some(second.handle.as_str())
    );
    assert_eq!(finish(&world.ctx, &world.need, GAVE_UP, ""), None);

    let outcomes: Vec<_> = history(&world)
        .into_iter()
        .map(|record| record.outcome)
        .collect();
    assert_eq!(
        outcomes,
        vec![Some("gave-up".to_owned()), Some("gave-up".to_owned())]
    );
}

#[test]
fn finish_replies_the_detail_on_the_record_before_closing_it() {
    let world = world();
    let record = world.mailspace.pump_send(None, &world.need, None).unwrap();
    world.host.reply(
        "pump list --status open --json",
        0,
        &format!(
            "[{{\"handle\": \"{}\", \"need\": \"{}\", \"status\": \"open\"}}]",
            record.handle, world.need
        ),
    );

    finish(&world.ctx, &world.need, GAVE_UP, "unit u1 is stuck");

    let commands = world.host.commands();
    assert!(
        commands
            .iter()
            .any(|cmd| cmd.contains(&format!("mail reply {} --from mind", record.handle))),
        "{commands:?}"
    );
}

fn withdraw_request(world: &World, reason: &str) {
    let mut store = PumpStore::open(&world.ctx.home.join(DB_FILENAME)).unwrap();
    store.add_withdraw_request(&world.need, reason, T0).unwrap();
}

#[test]
fn an_applied_withdraw_closes_the_record_withdrawn() {
    let world = world();
    world.mailspace.pump_send(None, &world.need, None).unwrap();
    let mut st = planned(&world.need, &[("u1", Status::Pending)]);
    withdraw_request(&world, "operator changed their mind");

    crate::pump::withdraw::apply_pending(&world.ctx, &mut st);

    assert_eq!(st.units["u1"].status, Status::Withdrawn);
    let done = only_record(&world);
    assert_eq!(done.status, PumpState::Done);
    assert_eq!(done.outcome.as_deref(), Some("withdrawn"));
}

fn replan_request(world: &World) {
    let mut store = PumpStore::open(&world.ctx.home.join(DB_FILENAME)).unwrap();
    store.add_replan_request(&world.need, false, T0).unwrap();
}

#[test]
fn an_applied_replan_closes_the_record_superseded_and_opens_a_fresh_one() {
    let world = world();
    let first = world.mailspace.pump_send(None, &world.need, None).unwrap();
    let mut st = planned(&world.need, &[("u1", Status::Pending)]);
    replan_request(&world);

    crate::pump::replan::apply_pending(&world.ctx, &mut st);

    let records = history(&world);
    assert_eq!(records.len(), 2, "{records:?}");
    assert_eq!(records[0].handle, first.handle);
    assert_eq!(records[0].status, PumpState::Done);
    assert_eq!(records[0].outcome.as_deref(), Some("superseded"));
    assert_eq!(records[1].status, PumpState::Open, "the replan is intake");
    assert!(
        !st.needs.contains_key(&world.need),
        "the plan is thrown away"
    );
}

#[test]
fn an_applied_replan_of_a_need_with_no_record_still_opens_one() {
    let world = world();
    let mut st = planned(&world.need, &[("u1", Status::Pending)]);
    replan_request(&world);

    crate::pump::replan::apply_pending(&world.ctx, &mut st);

    let record = only_record(&world);
    assert_eq!(record.status, PumpState::Open);
}

#[test]
fn a_refused_replan_leaves_the_record_alone() {
    let world = world();
    world.mailspace.pump_send(None, &world.need, None).unwrap();
    let mut st = planned(&world.need, &[("u1", Status::Inflight)]);
    replan_request(&world);

    crate::pump::replan::apply_pending(&world.ctx, &mut st);

    assert!(st.needs.contains_key(&world.need), "live work refuses");
    let record = only_record(&world);
    assert_eq!(record.status, PumpState::Open);
    assert_eq!(record.outcome, None);
}

/// The reason, if any, the pump has given up on `need`.
fn reason(world: &World, st: &State) -> Option<String> {
    gave_up_reason(&world.ctx, st, &world.need)
}

#[test]
fn a_planner_that_gave_up_parks_the_need() {
    let world = world();
    let mut st = State::default();
    assert_eq!(reason(&world, &st), None);
    st.board.planner_failed.push(world.need.clone());
    let why = reason(&world, &st).unwrap();
    assert!(why.contains("planner"), "{why}");
}

#[test]
fn a_stuck_unit_with_nothing_else_moving_parks_the_need() {
    let world = world();
    let st = planned(
        &world.need,
        &[("u1", Status::Merged), ("u2", Status::Stuck)],
    );
    let why = reason(&world, &st).unwrap();
    assert!(why.contains("unit u2 is stuck"), "{why}");
}

#[test]
fn a_stuck_unit_does_not_park_a_need_that_still_has_work_moving() {
    let world = world();
    for moving in [
        Status::Inflight,
        Status::Delivered,
        Status::Merging,
        Status::Quarantined,
        Status::Splitting,
        Status::Pending,
    ] {
        let st = planned(&world.need, &[("u1", Status::Stuck), ("u2", moving)]);
        assert_eq!(reason(&world, &st), None, "{moving:?}");
    }
}

#[test]
fn a_unit_waiting_only_on_a_stuck_unit_does_not_keep_the_need_alive() {
    let world = world();
    let mut st = planned(
        &world.need,
        &[("u1", Status::Stuck), ("u2", Status::Pending)],
    );
    st.units.get_mut("u2").unwrap().after = vec![String::from("u1")];
    assert!(reason(&world, &st).is_some());
    st.units.get_mut("u2").unwrap().after.clear();
    assert_eq!(
        reason(&world, &st),
        None,
        "an independent unit can still run"
    );
}

#[test]
fn a_need_being_planned_or_amended_is_not_given_up_on() {
    let world = world();
    let mut st = planned(&world.need, &[("u1", Status::Stuck)]);
    assert!(reason(&world, &st).is_some());
    st.board.revision_planning.insert(world.need.clone(), T0);
    assert_eq!(reason(&world, &st), None);
    st.board.revision_planning.clear();
    st.needs.get_mut(&world.need).unwrap().amendment_pending = true;
    assert_eq!(reason(&world, &st), None);
}

#[test]
fn a_need_audit_hold_parks_the_need() {
    let world = world();
    let mut st = planned(&world.need, &[("u1", Status::Merged)]);
    let hold = crate::pump::need_verdict::NeedAuditHold {
        reason: String::from("blocked finding remains"),
        ..Default::default()
    };
    let verdict = crate::pump::need_verdict::NeedVerdictState {
        hold: Some(hold),
        ..Default::default()
    };
    crate::pump::need_verdict::set_state(&mut st, "default", &world.need, &verdict).unwrap();
    let why = reason(&world, &st).unwrap();
    assert!(why.contains("blocked finding remains"), "{why}");
}

#[test]
fn settling_gave_up_closes_only_the_parked_needs_record() {
    let world = world();
    let record = world.mailspace.pump_send(None, &world.need, None).unwrap();
    let healthy = send_need(&world.mailspace, "healthy");
    let healthy_record = world.mailspace.pump_send(None, &healthy, None).unwrap();
    let mut st = planned(&world.need, &[("u1", Status::Stuck)]);
    st.needs.insert(healthy.clone(), NeedRecord::default());

    settle_gave_up(&world.ctx, &st, &[world.need.clone(), healthy.clone()]);

    let parked = history(&world);
    assert_eq!(parked[0].handle, record.handle);
    assert_eq!(parked[0].outcome.as_deref(), Some("gave-up"));
    let other = world.mailspace.pump_history(&healthy).unwrap();
    assert_eq!(other[0].handle, healthy_record.handle);
    assert_eq!(other[0].status, PumpState::Open);
}

#[test]
fn the_open_lookups_read_the_cli_list() {
    let world = world();
    world.host.reply(
        "pump list --status open --json",
        0,
        "noise\n[{\"handle\": \"p1\", \"need\": \"n1\"}, {\"handle\": \"p2\", \"need\": \"n2\"}]",
    );
    let needs = open_need_handles(&world.ctx).unwrap();
    assert_eq!(needs, ["n1".to_owned(), "n2".to_owned()].into());
    world.host.reply(
        "pump list --status open --json --need n2",
        0,
        "[{\"handle\": \"p2\", \"need\": \"n2\"}]",
    );
    assert_eq!(open_handle(&world.ctx, "n2").as_deref(), Some("p2"));
    world.host.reply("pump list", 1, "error");
    assert_eq!(open_need_handles(&world.ctx), None);
    assert_eq!(open_handle(&world.ctx, "n2"), None);
}

#[test]
fn a_context_without_a_mailspace_closes_nothing_and_does_not_fail() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    assert!(!Path::new(&ctx.project).exists());
    assert_eq!(finish(&ctx, "n1", WITHDRAWN, "x"), None);
    supersede(&ctx, "n1");
    settle_closed_needs(&ctx, &State::default(), &closed("n1"));
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("pump records unavailable")),
        "{:?}",
        host.logs()
    );
}

#[test]
fn an_amendment_opens_a_record_only_when_the_need_has_none() {
    let world = world();

    ensure_open(&world.ctx, &world.need);
    ensure_open(&world.ctx, &world.need);
    let first = only_record(&world);
    assert_eq!(first.status, PumpState::Open, "one record, not two");

    world
        .mailspace
        .pump_done(&first.handle, Some("landed"))
        .unwrap();
    ensure_open(&world.ctx, &world.need);

    let records = history(&world);
    assert_eq!(records.len(), 2, "the amended need is a second dispatch");
    assert_eq!(records[0].outcome.as_deref(), Some("landed"));
    assert_eq!(records[1].status, PumpState::Open);
}
