use super::MailspacePort;
use crate::letter::fixture::{body_with_deps, need_body, task_body};
use crate::letter::records::NoteWord;
use crate::mailspace::SendRequest;
use crate::mailspace::pump_note::note_body;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::slice_dispatch::SliceMail;

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

/// Send a need from `from` to `to` with `cc`, and return its handle.
fn need_from(p: &Project, from: &str, to: &[&str], cc: &[&str]) -> String {
    p.mailspace
        .send(SendRequest {
            from: from.into(),
            to: to.iter().map(|who| (*who).into()).collect(),
            cc: cc.iter().map(|who| (*who).into()).collect(),
            bcc: Vec::new(),
            subject: "a need".into(),
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

/// The `To` and `Cc` of every reply stored on `need`'s thread, each address
/// resolved the way the mailspace classifies recipients (local identity name
/// or remote address).
fn reply_recipients(p: &Project, need: &str) -> (Vec<String>, Vec<String>) {
    let storage = p.mailspace.storage().unwrap();
    let id = storage.resolve_message_token(need).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    let mut to = Vec::new();
    let mut cc = Vec::new();
    for child in storage.reply_child_contents(&view.content_id).unwrap() {
        let row = storage.message_by_content_id(&child).unwrap().unwrap();
        let bytes = storage.read_listed_blob(&row.blob_relpath).unwrap();
        let parsed = mail_parser::MessageParser::default().parse(&bytes).unwrap();
        for (header, out) in [(parsed.to(), &mut to), (parsed.cc(), &mut cc)] {
            if let Some(addresses) = header {
                for address in addresses.iter() {
                    let Some(value) = address.address.as_deref() else {
                        continue;
                    };
                    match p.mailspace.classify_recipient(value).unwrap() {
                        crate::mailspace::Recipient::Local(identity) => out.push(identity),
                        crate::mailspace::Recipient::Remote(remote) => out.push(remote),
                    }
                }
            }
        }
    }
    (to, cc)
}

#[test]
fn audience_post_self_addresses_when_empty() {
    let p = project();
    let port = port(&p);
    // A need from mind to mind: its whole audience is the pump identity, so
    // the note can only be stored as a self-addressed reply.
    let need = need_from(&p, "mind", &["mind"], &[]);
    port.post_to_need_audience(
        &need,
        &note_body(NoteWord::Deviations, Some("run-1"), "pump: run 1 started").unwrap(),
    )
    .unwrap();
    let found = port
        .notes_from(&need, NoteWord::Deviations, "mind")
        .unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].1.prose.trim(), "pump: run 1 started");
    assert_eq!(found[0].1.pump.reference.as_deref(), Some("run-1"));
    let (to, cc) = reply_recipients(&p, &need);
    assert_eq!(to, vec!["mind".to_owned()], "{to:?}");
    assert!(cc.is_empty(), "{cc:?}");
}

#[test]
fn an_audience_post_keeps_the_need_recipients() {
    let p = project();
    let port = port(&p);
    // Ordinary nonempty audience: the need's sender is still on the To list.
    let ordinary = need_from(&p, "mind", &["operator"], &[]);
    port.post_to_need_audience(&ordinary, "pump: run 2 started")
        .unwrap();
    let (to, cc) = reply_recipients(&p, &ordinary);
    assert_eq!(to, vec!["operator".to_owned()], "{to:?}");
    assert!(cc.is_empty(), "{cc:?}");
    // Cc-only audience: the Cc member is promoted to To, as before.
    let cc_only = need_from(&p, "mind", &["mind"], &["elsewhere"]);
    port.post_to_need_audience(&cc_only, "pump: run 3 started")
        .unwrap();
    let (to, cc) = reply_recipients(&p, &cc_only);
    assert_eq!(to, vec!["elsewhere".to_owned()], "{to:?}");
    assert!(cc.is_empty(), "{cc:?}");
}
