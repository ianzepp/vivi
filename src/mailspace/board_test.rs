use super::*;
use crate::mailspace::SendRequest;

fn roster() -> (Mailspace, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    (mailspace, tmp)
}

fn send(mailspace: &Mailspace, role: &str, subject: &str) -> String {
    mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: crate::letter::fixture::body_for(role.trim_end_matches('s'), "body"),
            role: role.into(),
            kind: Some(role.trim_end_matches('s').into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

fn options() -> BoardOptions {
    BoardOptions {
        wants_cap: 10,
        ..BoardOptions::default()
    }
}

fn cto(board: &Board) -> &IdentityBoard {
    board
        .identities
        .iter()
        .find(|identity| identity.identity == "cto")
        .unwrap()
}

#[test]
fn board_lists_open_need() {
    let (mailspace, _tmp) = roster();
    let handle = send(&mailspace, "needs", "ship the thing");
    let board = mailspace.board(&options()).unwrap();
    let cto = cto(&board);
    assert_eq!(cto.needs.len(), 1);
    assert_eq!(cto.needs[0].handle, handle);
    assert_eq!(cto.needs[0].subject, "ship the thing");
    assert!(cto.tasks.is_empty() && cto.wants.is_empty());
    assert_eq!(cto.actionable_open, 1);
    assert_eq!(board.totals.needs_open, 1);
    assert_eq!(board.totals.actionable_open, 1);
    assert!(board.graphs.is_none() && board.pump.is_none());
}

#[test]
fn board_filters_identity_and_adds_graph() {
    let (mailspace, _tmp) = roster();
    send(&mailspace, "needs", "ship the thing");
    let board = mailspace
        .board(&BoardOptions {
            for_identity: Some("cto".into()),
            with_graph: true,
            ..options()
        })
        .unwrap();
    assert_eq!(board.identities.len(), 1);
    let graphs = board.graphs.unwrap();
    assert!(graphs.iter().any(|graph| !graph.nodes.is_empty()));
}

#[test]
fn board_serializes_to_json_shape() {
    let (mailspace, _tmp) = roster();
    send(&mailspace, "wants", "nice to have");
    let board = mailspace.board(&options()).unwrap();
    let json = serde_json::to_value(&board).unwrap();
    assert_eq!(json["totals"]["wants_open"], 1);
    assert!(json["identities"].is_array());
    assert!(json["goals"].is_array());
    assert!(json.get("graphs").is_none());
}

#[test]
fn board_caps_wants_and_counts_hidden() {
    let (mailspace, _tmp) = roster();
    send(&mailspace, "wants", "one");
    send(&mailspace, "wants", "two");
    let board = mailspace
        .board(&BoardOptions {
            wants_cap: 1,
            ..options()
        })
        .unwrap();
    assert_eq!(cto(&board).wants.len(), 1);
    assert_eq!(cto(&board).wants_hidden, 1);
    assert_eq!(board.totals.wants_open, 2);
}

fn serving_need(mailspace: &Mailspace, want: &str) -> String {
    let mut front = crate::letter::work::NeedFront::new(vec!["done".to_string()]);
    front.serves = Some(want.to_string());
    mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "serving need".into(),
            body: crate::letter::fixture::need_body_with(&front, "prose"),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

fn close(mailspace: &Mailspace, handle: &str) {
    mailspace
        .move_item("ceo", handle, "done", None, "close", None)
        .unwrap();
}

#[test]
fn board_lists_a_want_with_its_open_serving_need() {
    let (mailspace, _tmp) = roster();
    let want = send(&mailspace, "wants", "wish");
    let need = serving_need(&mailspace, &want);
    let board = mailspace.board(&options()).unwrap();
    let cto = cto(&board);
    assert_eq!(cto.wants.len(), 1);
    assert_eq!(cto.wants[0].handle, want);
    assert_eq!(cto.wants[0].serving.len(), 1);
    assert_eq!(cto.wants[0].serving[0].handle, need);
    assert_eq!(cto.wants[0].serving[0].state, "open");
    assert_eq!(board.totals.wants_open, 1);
    let json = serde_json::to_value(&board).unwrap();
    let want_json = &json["identities"][1]["wants"][0];
    assert_eq!(want_json["serving"][0]["state"], "open");
    assert!(json["identities"][1]["needs"][0].get("serving").is_none());
}

#[test]
fn board_drops_a_want_when_its_serving_need_closes() {
    let (mailspace, _tmp) = roster();
    let want = send(&mailspace, "wants", "wish");
    let need = serving_need(&mailspace, &want);
    close(&mailspace, &need);
    let board = mailspace.board(&options()).unwrap();
    assert!(cto(&board).wants.is_empty());
    assert_eq!(board.totals.wants_open, 0);
}

#[test]
fn board_brings_back_a_hand_closed_want_that_gets_a_serving_need() {
    let (mailspace, _tmp) = roster();
    let want = send(&mailspace, "wants", "wish");
    close(&mailspace, &want);
    assert!(cto(&mailspace.board(&options()).unwrap()).wants.is_empty());
    let need = serving_need(&mailspace, &want);
    let board = mailspace.board(&options()).unwrap();
    let cto = cto(&board);
    assert_eq!(cto.wants.len(), 1);
    assert_eq!(cto.wants[0].handle, want);
    assert_eq!(cto.wants[0].serving[0].handle, need);
    assert_eq!(cto.wants[0].serving[0].state, "open");
    assert_eq!(board.totals.wants_open, 1);
}

#[test]
fn board_task_item_has_no_serving() {
    let (mailspace, _tmp) = roster();
    send(&mailspace, "tasks", "do it");
    let board = mailspace.board(&options()).unwrap();
    assert!(cto(&board).tasks[0].serving.is_empty());
    let json = serde_json::to_value(&board).unwrap();
    assert!(json["identities"][1]["tasks"][0].get("serving").is_none());
}
