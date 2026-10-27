use super::MailspacePort;
use crate::letter::fixture::{body_with_deps, need_body, task_body};
use crate::letter::records::NoteWord;
use crate::mailspace::pump_note::note_body;
use crate::mailspace::{Mailspace, Recipient, SendRequest};
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
    port.reply(&task, "hello").unwrap();
    let storage = p.mailspace.storage().unwrap();
    let id = storage.resolve_message_token(&task).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    let kids = storage.reply_child_contents(&view.content_id).unwrap();
    assert_eq!(kids.len(), 1);
}

#[test]
fn a_reply_on_a_pump_to_pump_step_is_stored_and_readable_as_marked() {
    use crate::pump::flow::slice_dispatch::SliceMail;

    let p = project();
    let port = port(&p);
    let task = port
        .send_step("mind", "mind", "t", &task_body("x"), &[])
        .unwrap();
    port.reply(&task, "pool result j1: finished").unwrap();
    let replies = port.marked_replies(&task, "pool result ").unwrap();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0].1, "pool result j1: finished");
}

/// A need sent from the pump identity to the pump identity (plus `cc`), as a
/// run post would be when nobody else holds the need; the delivered handle.
fn self_need(p: &Project, cc: &[&str], subject: &str) -> String {
    p.mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["mind".into()],
            cc: cc.iter().map(|who| (*who).to_string()).collect(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: need_body(&["it works"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

/// The sorted local identities in the To header of every reply on `need`.
fn reply_to_identities(p: &Project, need: &str) -> Vec<String> {
    let storage = p.mailspace.storage().unwrap();
    let id = storage.resolve_message_token(need).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    let mut found = Vec::new();
    for child in storage.reply_child_contents(&view.content_id).unwrap() {
        let row = storage.message_by_content_id(&child).unwrap().unwrap();
        let text = String::from_utf8_lossy(&storage.read_listed_blob(&row.blob_relpath).unwrap())
            .into_owned();
        let parsed = mail_parser::MessageParser::default()
            .parse(text.as_bytes())
            .unwrap_or_else(|| panic!("reply on {need} does not parse"));
        for address in parsed.to().into_iter().flatten() {
            let Some(value) = address.address.as_deref() else {
                continue;
            };
            if let Ok(Recipient::Local(identity)) = p.mailspace.classify_recipient(value) {
                found.push(identity);
            }
        }
    }
    found.sort();
    found
}

#[test]
fn audience_post_self_addresses_when_empty() {
    use crate::pump::flow::slice_dispatch::SliceMail;

    let p = project();
    let port = port(&p);
    let body = note_body(
        NoteWord::Attempt,
        Some("src/pump/flow/mail/send.rs"),
        "note",
    )
    .unwrap();

    // From mind to mind with nobody else on it: the post is self-addressed
    // and still stored as one logical note on the thread.
    let alone = self_need(&p, &[], "audience of one");
    port.post_to_need_audience(&alone, &body).unwrap();
    let notes = port.notes_from(&alone, NoteWord::Attempt, "mind").unwrap();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(
        notes[0].1.pump.reference.as_deref(),
        Some("src/pump/flow/mail/send.rs")
    );
    assert_eq!(reply_to_identities(&p, &alone), vec!["mind"]);

    // An ordinary nonempty audience keeps its recipient: the sender.
    let plain = p.need("plain audience");
    port.post_to_need_audience(&plain, &body).unwrap();
    assert_eq!(reply_to_identities(&p, &plain), vec!["operator"]);

    // A Cc-only audience is still promoted from Cc to To.
    let cc_only = self_need(&p, &["elsewhere"], "cc audience");
    port.post_to_need_audience(&cc_only, &body).unwrap();
    assert_eq!(reply_to_identities(&p, &cc_only), vec!["elsewhere"]);
}
