//! Tests for operator withdraws: the state change the pump applies, and the
//! CLI half that posts the action on the need's open pump record.

use std::collections::BTreeSet;
use std::fs;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::cli::PumpWithdrawCommand;
use crate::letter::fixture::{need_body_with, want_body};
use crate::letter::work::NeedFront;
use crate::mailspace::{Mailspace, RoleUpdate, SendRequest, WantState, held_item, is_done};
use crate::pump::config::PumpConfig;
use crate::pump::fixture::project;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{NeedRecord, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::pump::withdraw::{apply_one, run};

const T0: u64 = 1_000_000;

fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.set_now(T0);
    (dir, ctx, host)
}

fn add_need(st: &mut State, handle: &str, units: &[(&str, Status)]) {
    st.needs.insert(
        handle.to_owned(),
        NeedRecord {
            verdict: String::from("units"),
            units: units.iter().map(|(uid, _)| (*uid).to_owned()).collect(),
            revision: 1,
            ..NeedRecord::default()
        },
    );
    for (uid, status) in units {
        st.units.insert(
            (*uid).to_owned(),
            Unit {
                need: handle.to_owned(),
                target: String::from("default"),
                revision: 1,
                status: *status,
                ..Unit::default()
            },
        );
    }
}

fn status(st: &State, uid: &str) -> Status {
    st.units[uid].status
}

fn exceptions(ctx: &Ctx) -> String {
    fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap_or_default()
}

fn log(host: &FakeHost) -> String {
    host.logs().join("\n")
}

#[test]
fn applying_a_request_withdraws_unfinished_units_and_closes_the_need() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    add_need(
        &mut st,
        "pumpfix-g1",
        &[
            ("pending", Status::Pending),
            ("stuck", Status::Stuck),
            ("delivered", Status::Delivered),
            ("merged", Status::Merged),
            ("nodefect", Status::NoDefect),
            ("replaced", Status::Replaced),
        ],
    );
    add_need(&mut st, "other", &[("o1", Status::Pending)]);

    let text = apply_one(&ctx, &mut st, "pumpfix-g1", "15-era repair need");

    assert_eq!(
        text,
        "withdrew need pumpfix-g1: 3 unit(s) (15-era repair need)"
    );

    assert_eq!(status(&st, "pending"), Status::Withdrawn);
    assert_eq!(status(&st, "stuck"), Status::Withdrawn);
    assert_eq!(status(&st, "delivered"), Status::Withdrawn);
    assert_eq!(status(&st, "merged"), Status::Merged);
    assert_eq!(status(&st, "nodefect"), Status::NoDefect);
    assert_eq!(status(&st, "replaced"), Status::Replaced);
    assert_eq!(status(&st, "o1"), Status::Pending);
    assert_eq!(st.closed_needs, vec![String::from("pumpfix-g1")]);
    assert!(log(&host).contains("withdrew need pumpfix-g1: 3 unit(s) (15-era repair need)"));
    assert!(!log(&host).contains("mail need is not closed"));
    assert!(exceptions(&ctx).contains("need-withdrawn pumpfix-g1"));
}

#[test]
fn a_need_the_pump_holds_nothing_for_changes_no_state_but_still_ends_its_dispatch() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    add_need(&mut st, "n1", &[("u1", Status::Pending)]);

    let text = apply_one(&ctx, &mut st, "nope", "typo");

    assert_eq!(status(&st, "u1"), Status::Pending);
    assert!(st.closed_needs.is_empty());
    assert!(
        text.contains("the pump held no plan for it (typo)"),
        "{text}"
    );
    assert!(log(&host).contains("withdraw nope: unknown need; closing its record"));
    assert!(!exceptions(&ctx).contains("need-withdrawn"));
}

#[test]
fn applying_the_same_need_twice_closes_it_once() {
    let (_dir, ctx, _) = setup();
    let mut st = State::default();
    add_need(&mut st, "n1", &[("u1", Status::Stuck)]);

    apply_one(&ctx, &mut st, "n1", "first");
    apply_one(&ctx, &mut st, "n1", "second");

    assert_eq!(st.closed_needs, vec![String::from("n1")]);
}

fn command(needs: &[&str], reason: &str) -> PumpWithdrawCommand {
    PumpWithdrawCommand {
        needs: needs.iter().map(|need| (*need).to_owned()).collect(),
        reason: reason.to_owned(),
        project: None,
    }
}

#[test]
fn run_withdraw_posts_one_action_per_need_on_its_open_record() {
    let project = project();
    let first = project.need("fix auth");
    let second = project.need("fix billing");
    let record = project.mailspace.pump_send(None, &first, None).unwrap();
    project.mailspace.pump_send(None, &second, None).unwrap();
    let mut store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    store.save(&State::default()).unwrap();

    run(project.root(), &command(&[&first, &second], "stale")).unwrap();

    let actions = project
        .mailspace
        .pump_pending_actions(&BTreeSet::new())
        .unwrap();
    let posted: Vec<(&str, &str)> = actions
        .iter()
        .map(|action| (action.need.as_str(), action.request.reason.as_str()))
        .collect();
    assert_eq!(
        posted,
        vec![(first.as_str(), "stale"), (second.as_str(), "stale")]
    );
    assert_eq!(actions[0].record, record.handle);
    for need in [&first, &second] {
        assert!(is_closed(&project.mailspace, need), "{need} not closed");
        assert_eq!(
            verdict_of(&project.mailspace, need).as_deref(),
            Some("withdrawn")
        );
        let state = project.mailspace.item_event_state(need).unwrap();
        assert_eq!(state.map(|s| s.state).as_deref(), Some("done"));
    }
}

#[test]
fn run_withdraw_does_not_close_a_need_twice_but_still_posts_its_action() {
    let project = project();
    let need = project.need("fix auth");
    project.mailspace.pump_send(None, &need, None).unwrap();
    let mut store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    store.save(&State::default()).unwrap();
    project
        .mailspace
        .move_item("mind", &need, "done", None, "need done", None)
        .unwrap();
    assert!(is_closed(&project.mailspace, &need));

    run(project.root(), &command(&[&need], "stale")).unwrap();

    assert_eq!(verdict_of(&project.mailspace, &need), None);
    let actions = project
        .mailspace
        .pump_pending_actions(&BTreeSet::new())
        .unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].request.reason, "stale");
}

fn is_closed(mailspace: &Mailspace, handle: &str) -> bool {
    let storage = mailspace.storage().unwrap();
    is_done(&held_item(&storage, handle).unwrap())
}

fn verdict_of(mailspace: &Mailspace, handle: &str) -> Option<String> {
    let storage = mailspace.storage().unwrap();
    let item = held_item(&storage, handle).unwrap();
    item.rows.iter().find_map(|row| {
        storage
            .item_metadata(&row.message_id)
            .unwrap()
            .get("verdict")
            .filter(|v| !v.is_empty())
            .cloned()
    })
}

fn send_work(mailspace: &Mailspace, kind: &str, body: String) -> String {
    mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: format!("{kind} subject"),
            body,
            role: format!("{kind}s"),
            kind: Some(kind.into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

#[test]
fn a_want_whose_only_serving_need_was_withdrawn_is_done() {
    let project = project();
    let want = send_work(&project.mailspace, "want", want_body("q", "p"));
    let mut front = NeedFront::new(vec!["done".to_string()]);
    front.serves = Some(want.clone());
    let need = send_work(&project.mailspace, "need", need_body_with(&front, "prose"));
    project.mailspace.pump_send(None, &need, None).unwrap();
    let mut store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    store.save(&State::default()).unwrap();
    assert_eq!(
        project.mailspace.want_state(&want).unwrap().state,
        WantState::Open
    );

    run(project.root(), &command(&[&need], "stale")).unwrap();

    let report = project.mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, WantState::Done);
    assert_eq!(report.serving[0].verdict.as_deref(), Some("withdrawn"));
}

#[test]
fn run_withdraw_refuses_a_need_without_an_open_record_and_posts_nothing() {
    let project = project();
    let first = project.need("fix auth");
    let second = project.need("fix billing");
    project.mailspace.pump_send(None, &first, None).unwrap();
    let mut store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    store.save(&State::default()).unwrap();

    let error = run(project.root(), &command(&[&first, &second], "stale")).unwrap_err();

    let text = error.to_string();
    assert!(
        text.contains(&format!("need {second} has no open pump record")),
        "{text}"
    );
    assert!(text.contains("vivi pump send"), "{text}");
    assert!(
        project
            .mailspace
            .pump_pending_actions(&BTreeSet::new())
            .unwrap()
            .is_empty()
    );
    assert!(!is_closed(&project.mailspace, &first));
}

#[test]
fn run_withdraw_refuses_a_blank_reason_and_a_missing_database() {
    let project = project();
    assert!(run(project.root(), &command(&["n1"], "  ")).is_err());
    let error = run(project.root(), &command(&["n1"], "why")).unwrap_err();
    assert!(error.to_string().contains("no pump.db"));
}

#[test]
fn run_withdraw_closes_a_need_an_assignee_reported_done_but_nobody_accepted() {
    let mut project = project();
    project
        .mailspace
        .set_role(
            "elsewhere",
            RoleUpdate {
                kind: Some(Some("hand".into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    let want = send_work(&project.mailspace, "want", want_body("q", "p"));
    let mut front = NeedFront::new(vec!["done".to_string()]);
    front.serves = Some(want.clone());
    let need = project
        .mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["elsewhere".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "need subject".into(),
            body: need_body_with(&front, "prose"),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    project.mailspace.pump_send(None, &need, None).unwrap();
    let mut store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    store.save(&State::default()).unwrap();
    let outcome = project
        .mailspace
        .move_item_outcome("elsewhere", &need, "done", None, "need done", None)
        .unwrap();
    assert!(outcome.reported, "{outcome:?}");
    assert!(is_closed(&project.mailspace, &need), "the folder says done");
    assert_eq!(
        project.mailspace.want_state(&want).unwrap().state,
        WantState::Open,
        "an unaccepted report leaves the want open"
    );

    run(project.root(), &command(&[&need], "stale")).unwrap();

    assert_eq!(
        verdict_of(&project.mailspace, &need).as_deref(),
        Some("withdrawn")
    );
    let report = project.mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, WantState::Done);
    assert_eq!(report.serving[0].verdict.as_deref(), Some("withdrawn"));
}
