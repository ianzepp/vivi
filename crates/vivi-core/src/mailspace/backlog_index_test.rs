use super::BacklogIndex;
use crate::mailspace::backlog::backlog_node_by_token;
use crate::mailspace::{Mailspace, SendRequest};
use vivi_mail::letter::fixture::need_body;
use vivi_storage::storage::{BACKLOG_GRAPH_CODE, Storage};

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("hand").unwrap();
    (tmp, mailspace)
}

fn send_need(mailspace: &Mailspace) -> String {
    mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "need".into(),
            body: need_body(&["done"], "prose"),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

fn nodes_of(storage: &Storage) -> Vec<vivi_storage::storage::WorkGraphNodeRow> {
    let graph = storage
        .work_graph_by_code(BACKLOG_GRAPH_CODE)
        .unwrap()
        .unwrap();
    storage.work_graph_nodes(&graph.handle).unwrap()
}

#[test]
fn a_mailspace_with_no_backlog_graph_has_an_empty_index() {
    let (_tmp, mailspace) = space();
    let storage = mailspace.storage().unwrap();
    let index = BacklogIndex::load(&storage).unwrap();
    assert!(index.node_by_token(&storage, "nothing").unwrap().is_none());
}

#[test]
fn the_index_answers_every_token_as_the_linear_lookup_does() {
    let (_tmp, mailspace) = space();
    let handles: Vec<String> = (0..6).map(|_| send_need(&mailspace)).collect();
    let storage = mailspace.storage().unwrap();
    let nodes = nodes_of(&storage);
    let index = BacklogIndex::load(&storage).unwrap();

    let mut tokens = handles.clone();
    for handle in &handles {
        tokens.push(storage.resolve_message_token(handle).unwrap());
    }
    tokens.push("no-such-token".into());
    for token in &tokens {
        let linear = backlog_node_by_token(&storage, &nodes, token).unwrap();
        let indexed = index.node_by_token(&storage, token).unwrap();
        assert_eq!(indexed, linear.as_ref(), "token {token}");
    }
    assert!(
        index
            .node_by_token(&storage, &handles[0])
            .unwrap()
            .is_some(),
        "a sent need has a backlog node"
    );
}

#[test]
fn a_full_message_id_finds_the_node_minted_under_its_short_prefix() {
    let (_tmp, mailspace) = space();
    let handle = send_need(&mailspace);
    let storage = mailspace.storage().unwrap();
    let message_id = storage.resolve_message_token(&handle).unwrap();
    let index = BacklogIndex::load(&storage).unwrap();

    let by_handle = index.node_by_token(&storage, &handle).unwrap().unwrap();
    let by_id = index.node_by_token(&storage, &message_id).unwrap().unwrap();

    assert_eq!(by_handle.handle, by_id.handle);
    assert_ne!(
        message_id, by_handle.source_id,
        "the id is longer than the source id"
    );
}
