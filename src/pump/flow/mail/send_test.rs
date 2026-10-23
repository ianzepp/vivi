use super::MailspacePort;
use crate::pump::fixture::{Project, project};

fn port(p: &Project) -> MailspacePort {
    MailspacePort {
        mailspace: p.mailspace.clone(),
        pump_identity: "mind".into(),
    }
}

/// The unit-header count of every `bound` letter that replies to `need`.
fn bound_letters(p: &Project, need: &str) -> Vec<usize> {
    let storage = p.mailspace.storage().unwrap();
    let id = storage.resolve_message_token(need).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    let mut found = Vec::new();
    for child in storage.reply_child_contents(&view.content_id).unwrap() {
        let row = storage.message_by_content_id(&child).unwrap().unwrap();
        let text = String::from_utf8_lossy(&storage.read_listed_blob(&row.blob_relpath).unwrap())
            .into_owned();
        if text.contains("X-Vivi-Event: bound") {
            found.push(text.matches("X-Vivi-Event-Unit:").count());
        }
    }
    found
}

#[test]
fn a_step_sent_with_a_dependency_has_the_edge_in_the_backlog() {
    let p = project();
    let port = port(&p);
    let first = port
        .send_step("mind", "operator", "first", "x", &[])
        .unwrap();
    let second = port
        .send_step("mind", "operator", "second", "y", &[first.clone()])
        .unwrap();
    let deps = p.mailspace.backlog_dependencies(&second).unwrap();
    assert_eq!(deps.len(), 1, "{deps:?}");
    assert_eq!(deps[0].handle, first);
}

#[test]
fn one_bind_writes_one_bound_letter_naming_every_unit() {
    let p = project();
    let port = port(&p);
    let need = p.need("bind me");
    let a = port.send_step("mind", "operator", "a", "x", &[]).unwrap();
    let b = port.send_step("mind", "operator", "b", "x", &[]).unwrap();
    port.bind(&need, &[a, b]).unwrap();
    assert_eq!(bound_letters(&p, &need), vec![2]);
}

#[test]
fn binding_the_same_unit_to_the_same_need_again_is_idempotent() {
    let p = project();
    let port = port(&p);
    let need = p.need("bind twice");
    let a = port.send_step("mind", "operator", "a", "x", &[]).unwrap();
    let b = port.send_step("mind", "operator", "b", "x", &[]).unwrap();
    port.bind(&need, &[a.clone()]).unwrap();
    port.bind(&need, &[a.clone(), b.clone()]).unwrap();
    let storage = p.mailspace.storage().unwrap();
    let graph = storage
        .work_graph_by_code(crate::storage::BACKLOG_GRAPH_CODE)
        .unwrap()
        .unwrap();
    let mut bound: Vec<String> = storage
        .work_graph_nodes_by_subgraph(&graph.handle, &need)
        .unwrap()
        .into_iter()
        .map(|node| node.source_id)
        .collect();
    bound.sort();
    let mut want = vec![a, b];
    want.sort();
    assert_eq!(bound, want);
}

#[test]
fn a_reply_lands_on_the_task() {
    let p = project();
    let port = port(&p);
    let task = port.send_step("mind", "operator", "t", "x", &[]).unwrap();
    port.reply(&task, "pump-attempt: hello").unwrap();
    let storage = p.mailspace.storage().unwrap();
    let id = storage.resolve_message_token(&task).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    let kids = storage.reply_child_contents(&view.content_id).unwrap();
    assert_eq!(kids.len(), 1);
}
