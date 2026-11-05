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
            body: vivi_mail::letter::fixture::body_for(role.trim_end_matches('s'), "body"),
            role: role.into(),
            kind: Some(role.trim_end_matches('s').into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

fn send_unaddressed(mailspace: &Mailspace, from: &str, role: &str, subject: &str) -> String {
    mailspace
        .send(SendRequest {
            from: from.into(),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: vivi_mail::letter::fixture::body_for(role.trim_end_matches('s'), "body"),
            role: role.into(),
            kind: Some(role.trim_end_matches('s').into()),
            reply_to: None,
        })
        .unwrap()
        .sent
}

fn hold_work(
    mailspace: &Mailspace,
    account: &str,
    role: &str,
    kind: &str,
    subject: &str,
) -> String {
    let body = vivi_mail::letter::fixture::body_for(kind, "body");
    let bytes = format!(
        "From: sender@remote.example\r\nTo: {account}\r\nSubject: {subject}\r\n\
         Message-ID: <{subject}@remote.example>\r\nX-Vivi-Kind: {kind}\r\n\r\n{body}"
    );
    let storage = mailspace.storage().unwrap();
    let stored = storage
        .ingest_message(
            &vivi_storage::storage::MessageIngestRequest {
                account: account.into(),
                local_role: role.into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("{account}\0{subject}"),
                remote: None,
            },
            bytes.as_bytes(),
        )
        .unwrap();
    storage.display_handle(&stored.message_id).unwrap()
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
fn board_includes_author_only_and_non_roster_work() {
    let (mailspace, _tmp) = roster();
    let ceo_want = send_unaddressed(&mailspace, "ceo", "wants", "ceo wish");
    let cto_task = send_unaddressed(&mailspace, "cto", "tasks", "cto task");
    let addressed = send(&mailspace, "tasks", "addressed task");
    let ghost = hold_work(&mailspace, "ghost", "needs", "need", "ghost need");
    let foreign = hold_work(
        &mailspace,
        "agent@remote.example",
        "tasks",
        "task",
        "foreign task",
    );

    let board = mailspace
        .board(&BoardOptions {
            with_process: true,
            ..options()
        })
        .unwrap();
    let ceo = board
        .identities
        .iter()
        .find(|row| row.identity == "ceo")
        .unwrap();
    let cto = cto(&board);
    let ghost_board = board
        .identities
        .iter()
        .find(|row| row.identity == "ghost")
        .unwrap();
    let foreign_board = board
        .identities
        .iter()
        .find(|row| row.identity == "agent@remote.example")
        .unwrap();
    assert_eq!(
        ceo.wants
            .iter()
            .map(|item| item.handle.as_str())
            .collect::<Vec<_>>(),
        [ceo_want.as_str()]
    );
    assert_eq!(
        cto.tasks
            .iter()
            .filter(|item| item.handle == cto_task)
            .count(),
        1
    );
    assert_eq!(
        cto.tasks
            .iter()
            .filter(|item| item.handle == addressed)
            .count(),
        1
    );
    assert!(ceo.tasks.iter().all(|item| item.handle != addressed));
    assert_eq!(ghost_board.needs[0].handle, ghost);
    assert_eq!(foreign_board.tasks[0].handle, foreign);
    assert_eq!(foreign_board.address, "agent@remote.example");
    for non_roster in [ghost_board, foreign_board] {
        assert!(non_roster.model.is_none() && non_roster.thinking.is_none());
        assert!(non_roster.process.is_none());
    }
    assert_eq!(board.totals.wants_open, 1);
    assert_eq!(board.totals.tasks_open, 3);
    assert_eq!(board.totals.needs_open, 1);

    let filtered = mailspace
        .board(&BoardOptions {
            for_identity: Some("ceo".into()),
            ..options()
        })
        .unwrap();
    assert_eq!(filtered.identities.len(), 1);
    assert_eq!(filtered.identities[0].identity, "ceo");
    assert_eq!(filtered.identities[0].wants[0].handle, ceo_want);
    assert_eq!(filtered.totals.tasks_open, 0);
    assert_eq!(filtered.totals.needs_open, 0);
}

#[test]
fn unaddressed_want_follows_serving_needs() {
    let (mailspace, _tmp) = roster();
    let want = send_unaddressed(&mailspace, "ceo", "wants", "author-only wish");
    let report = mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, crate::mailspace::want_state::WantState::Open);
    assert!(report.serving.is_empty());

    let need = serving_need(&mailspace, &want);
    let board = mailspace.board(&options()).unwrap();
    let author = board
        .identities
        .iter()
        .find(|identity| identity.identity == "ceo")
        .unwrap();
    let item = &author.wants[0];
    assert_eq!(item.handle, want);
    assert_eq!(item.serving[0].handle, need);
    assert_eq!(item.serving[0].state, "open");

    close(&mailspace, &need);
    assert_eq!(
        mailspace.want_state(&want).unwrap().state,
        crate::mailspace::want_state::WantState::Done
    );
    let board = mailspace.board(&options()).unwrap();
    assert!(
        board
            .identities
            .iter()
            .find(|identity| identity.identity == "ceo")
            .unwrap()
            .wants
            .is_empty()
    );
    mailspace
        .move_item("ceo", &need, "needs", None, "need reopen", None)
        .unwrap();
    assert_eq!(
        mailspace.want_state(&want).unwrap().state,
        crate::mailspace::want_state::WantState::Open
    );
    let board = mailspace.board(&options()).unwrap();
    let author = board
        .identities
        .iter()
        .find(|identity| identity.identity == "ceo")
        .unwrap();
    assert_eq!(author.wants[0].serving[0].state, "open");
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
    assert!(board.graphs.is_none());
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
    let mut front = vivi_mail::letter::work::NeedFront::new(vec!["done".to_string()]);
    front.serves = Some(want.to_string());
    mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "serving need".into(),
            body: vivi_mail::letter::fixture::need_body_with(&front, "prose"),
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
