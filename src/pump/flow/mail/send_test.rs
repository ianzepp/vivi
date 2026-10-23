use super::MailspacePort;
use crate::letter::fixture::{body_with_deps, task_body};
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
        .send_step("mind", "operator", "first", &task_body("x"), &[])
        .unwrap();
    let second = port
        .send_step(
            "mind",
            "operator",
            "second",
            &body_with_deps("task", "y", std::slice::from_ref(&first)),
            &[],
        )
        .unwrap();
    let deps = p.mailspace.backlog_dependencies(&second).unwrap();
    assert_eq!(deps.len(), 1, "{deps:?}");
    assert_eq!(deps[0].handle, first);
}

#[test]
fn the_depends_on_parameter_is_merged_into_the_front_matter_without_duplicates() {
    let p = project();
    let port = port(&p);
    let first = port
        .send_step("mind", "operator", "first", &task_body("x"), &[])
        .unwrap();
    let second = port
        .send_step(
            "mind",
            "operator",
            "second",
            &body_with_deps("task", "y", std::slice::from_ref(&first)),
            std::slice::from_ref(&first),
        )
        .unwrap();
    let deps = p.mailspace.backlog_dependencies(&second).unwrap();
    assert_eq!(deps.len(), 1, "{deps:?}");
    let third = port
        .send_step(
            "mind",
            "operator",
            "third",
            &task_body("z"),
            std::slice::from_ref(&first),
        )
        .unwrap();
    let edges: Vec<(String, String)> = p
        .mailspace
        .graph_show("backlog")
        .unwrap()
        .edges
        .into_iter()
        .map(|edge| (edge.from_source_id, edge.to_source_id))
        .collect();
    assert_eq!(edges.len(), 2, "{edges:?}");
    assert!(edges.contains(&(first.clone(), second)), "{edges:?}");
    assert!(edges.contains(&(first, third)), "{edges:?}");
}

#[test]
fn a_step_body_without_front_matter_is_an_error() {
    let p = project();
    let err = port(&p)
        .send_step("mind", "operator", "prose", "just prose", &[])
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("step task body must open with front matter"),
        "{err}"
    );
}

#[test]
fn one_bind_writes_one_bound_letter_naming_every_unit() {
    let p = project();
    let port = port(&p);
    let need = p.need("bind me");
    let a = port
        .send_step("mind", "operator", "a", &task_body("x"), &[])
        .unwrap();
    let b = port
        .send_step("mind", "operator", "b", &task_body("x"), &[])
        .unwrap();
    port.bind(&need, &[a, b]).unwrap();
    assert_eq!(bound_letters(&p, &need), vec![2]);
}

#[test]
fn binding_the_same_unit_to_the_same_need_again_is_idempotent() {
    let p = project();
    let port = port(&p);
    let need = p.need("bind twice");
    let a = port
        .send_step("mind", "operator", "a", &task_body("x"), &[])
        .unwrap();
    let b = port
        .send_step("mind", "operator", "b", &task_body("x"), &[])
        .unwrap();
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
    let task = port
        .send_step("mind", "operator", "t", &task_body("x"), &[])
        .unwrap();
    port.reply(&task, "pump-attempt: hello").unwrap();
    let storage = p.mailspace.storage().unwrap();
    let id = storage.resolve_message_token(&task).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    let kids = storage.reply_child_contents(&view.content_id).unwrap();
    assert_eq!(kids.len(), 1);
}

#[test]
fn a_reply_on_a_pump_to_pump_step_is_stored_and_readable_as_marked() {
    use crate::mailspace::marked_line;
    use crate::pump::flow::slice_dispatch::SliceMail;

    let p = project();
    let port = port(&p);
    let task = port
        .send_step("mind", "mind", "t", &task_body("x"), &[])
        .unwrap();
    port.reply(&task, &marked_line("pump-attempt:", "hello"))
        .unwrap();
    let replies = port.marked_replies(&task, "pump-attempt:").unwrap();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0].1, "pump-attempt: hello");
    let from_pump = port
        .marked_replies_from(&task, "pump-attempt:", "mind")
        .unwrap();
    assert_eq!(from_pump.len(), 1, "{from_pump:?}");
}
