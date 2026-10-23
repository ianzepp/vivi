use super::*;

#[test]
fn strips_stacked_reply_prefixes() {
    assert_eq!(strip_reply_prefix("Re: Re[2]: status"), "status");
}

#[test]
fn reply_prefix_is_case_insensitive() {
    assert_eq!(strip_reply_prefix("rE: Status"), "status");
}

fn star(children: usize) -> BTreeMap<String, MailspaceLink> {
    (0..children)
        .map(|n| {
            let child = format!("child-{n}");
            let link = MailspaceLink {
                child_content_id: child.clone(),
                parent_content_id: "seed".into(),
                source: "captured".into(),
            };
            (child, link)
        })
        .collect()
}

#[test]
fn the_limit_caps_the_thread_even_when_neighbours_are_queued() {
    let links = star(5);

    for limit in [1, 2, 3] {
        let included = connected_content_ids("seed", &links, limit, 50);
        assert_eq!(included.len(), limit, "limit {limit}: {included:?}");
        assert!(included.contains("seed"), "the seed is always first");
    }
    assert_eq!(connected_content_ids("seed", &links, 6, 50).len(), 6);
    assert_eq!(connected_content_ids("seed", &links, 50, 50).len(), 6);
    assert!(connected_content_ids("seed", &links, 0, 50).is_empty());
}

#[test]
fn thread_bodies_come_back_with_lf_line_ends() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = crate::mailspace::Mailspace::init(Some(tmp.path())).unwrap();
    space.add_identity("mind").unwrap();
    space.add_identity("hand").unwrap();
    let request = |reply_to: Option<String>, body: &str| crate::mailspace::SendRequest {
        from: "mind".into(),
        to: vec!["hand".into()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "multi".into(),
        body: crate::letter::fixture::body_for("task", body),
        role: "tasks".into(),
        kind: Some("task".into()),
        reply_to,
        depends_on: Vec::new(),
    };
    let task = space.send(request(None, "one\ntwo\nthree\n")).unwrap();
    let handle = task.delivered[0].handle.clone();
    space
        .send(request(Some(handle.clone()), "alpha\nbeta\n\ngamma\n"))
        .unwrap();

    let messages = space.thread(&handle, false, 50, 50).unwrap();

    assert!(messages.len() >= 2, "{messages:?}");
    for message in &messages {
        assert!(!message.body.contains('\r'), "{:?}", message.body);
    }
    assert!(
        messages
            .iter()
            .any(|m| m.body.contains("alpha\nbeta\n\ngamma"))
    );
}

#[test]
fn text_body_turns_crlf_into_lf() {
    let raw = b"From: a@b.c\r\nTo: d@e.f\r\nSubject: s\r\n\r\nx\r\ny\r\n\r\nz\r\n";
    assert_eq!(text_body(raw), "x\ny\n\nz\n");
}
