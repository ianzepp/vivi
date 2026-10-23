use crate::letter::fixture::{body_with_deps, need_body, task_body_with};
use crate::letter::work::{NeedFront, TaskFront};
use crate::mailspace::{Mailspace, SendRequest};

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("hand").unwrap();
    (tmp, mailspace)
}

fn work(kind: &str, body: String) -> SendRequest {
    SendRequest {
        from: "mind".into(),
        to: vec!["hand".into()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "s".into(),
        body,
        role: format!("{kind}s"),
        kind: Some(kind.into()),
        reply_to: None,
    }
}

fn stored(mailspace: &Mailspace, handle: &str) -> String {
    let storage = mailspace.storage().unwrap();
    String::from_utf8(storage.read_message(handle).unwrap()).unwrap()
}

fn edges(mailspace: &Mailspace) -> Vec<(String, String)> {
    mailspace
        .graph_show("backlog")
        .unwrap()
        .edges
        .into_iter()
        .map(|edge| (edge.from_source_id, edge.to_source_id))
        .collect()
}

#[test]
fn a_front_matter_need_is_stored_with_the_format_header_and_no_dependency_header() {
    let (_tmp, mailspace) = space();
    let result = mailspace
        .send(work("need", need_body(&["done"], "prose")))
        .unwrap();
    let eml = stored(&mailspace, &result.delivered[0].handle);
    assert!(eml.contains("X-Vivi-Format: 1"), "{eml}");
    assert!(eml.contains("X-Vivi-Kind: need"), "{eml}");
    assert!(!eml.contains("X-Vivi-Depends-On"), "{eml}");
}

#[test]
fn a_need_without_done_when_is_refused_and_nothing_is_stored() {
    let (_tmp, mailspace) = space();
    let body = crate::letter::render(&NeedFront::default(), "prose").unwrap();
    let err = mailspace.send(work("need", body)).unwrap_err().to_string();
    assert!(err.contains("need has no done_when"), "{err}");
    assert!(
        mailspace
            .storage()
            .unwrap()
            .list_messages()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_front_matter_task_gets_its_backlog_edge() {
    let (_tmp, mailspace) = space();
    let first = mailspace
        .send(work("need", need_body(&["done"], "p")))
        .unwrap()
        .delivered[0]
        .handle
        .clone();
    let body = body_with_deps("task", "p", std::slice::from_ref(&first));
    let task = mailspace.send(work("task", body)).unwrap().delivered[0]
        .handle
        .clone();
    assert_eq!(edges(&mailspace), vec![(first, task)]);
}

#[test]
fn a_front_matter_task_stores_its_dependency_list_and_edges() {
    let (_tmp, mailspace) = space();
    let mut handles = Vec::new();
    for _ in 0..2 {
        handles.push(
            mailspace
                .send(work("need", need_body(&["done"], "p")))
                .unwrap()
                .delivered[0]
                .handle
                .clone(),
        );
    }
    let front = TaskFront {
        depends_on: handles.clone(),
        ..TaskFront::default()
    };
    let body = task_body_with(&front, "p");
    let task = mailspace.send(work("task", body)).unwrap().delivered[0]
        .handle
        .clone();
    let eml = stored(&mailspace, &task);
    assert!(!eml.contains("X-Vivi-Depends-On"), "{eml}");
    let listed = format!("depends_on = [\"{}\", \"{}\"]", handles[0], handles[1]);
    assert!(eml.contains(&listed), "{eml}");
    assert_eq!(edges(&mailspace).len(), 2);
}

#[test]
fn a_prose_need_is_still_stored_without_the_format_header() {
    // TRANSITIONAL (W3-B2): W3B2-38 refuses a prose work body; this test then
    // pins the refusal instead.
    let (_tmp, mailspace) = space();
    let result = mailspace
        .send(work("need", "done_when: x\n".into()))
        .unwrap();
    let eml = stored(&mailspace, &result.delivered[0].handle);
    assert!(!eml.contains("X-Vivi-Format"), "{eml}");
    assert!(eml.contains("X-Vivi-Kind: need"), "{eml}");
}
