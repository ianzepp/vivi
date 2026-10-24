//! Tests for the `vivi pump withdraw` CLI half: it closes the need and posts
//! the action on the need's open pump record.

use std::collections::BTreeSet;

use crate::cli::PumpWithdrawCommand;
use crate::letter::fixture::{need_body_with, want_body};
use crate::letter::work::NeedFront;
use crate::mailspace::{Mailspace, RoleUpdate, SendRequest, WantState, held_item, is_done};
use crate::pump::fixture::project;
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::pump::withdraw::run;

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
