//! Property and assignment letters are format-1 events. Their facts stay in
//! the event-protocol headers, so an empty front stores the prose with no fence.

use super::super::held_item;
use super::gated_event_body;
use crate::letter::plain::CloseFront;
use crate::letter::{Front, LetterKind, of_message};
use crate::mailspace::{AbsorbConfig, Mailspace, SendRequest};

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    for role in ["mind", "hand", "other"] {
        mailspace.add_identity(role).unwrap();
    }
    (tmp, mailspace)
}

fn send_work(mailspace: &Mailspace, kind: &str, subject: &str) -> (String, String) {
    let result = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: crate::letter::fixture::body_for(kind, "body"),
            role: format!("{kind}s"),
            kind: Some(kind.into()),
            reply_to: None,
        })
        .unwrap();
    (result.sent, result.delivered[0].handle.clone())
}

fn read(mailspace: &Mailspace, handle: &str) -> Vec<u8> {
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(handle).unwrap();
    storage.read_message(&id).unwrap()
}

fn event_letters(mailspace: &Mailspace, verb: &str) -> Vec<Vec<u8>> {
    let storage = mailspace.storage().unwrap();
    let header = format!("X-Vivi-Event: {verb}");
    storage
        .list_messages()
        .unwrap()
        .into_iter()
        .filter_map(|row| {
            let bytes = storage.read_message(&row.message_id).ok()?;
            let text = String::from_utf8_lossy(&bytes);
            text.lines()
                .any(|line| line.trim().eq_ignore_ascii_case(&header))
                .then_some(bytes)
        })
        .collect()
}

fn header(bytes: &[u8], name: &str) -> Option<String> {
    let parsed = mail_parser::MessageParser::default().parse(bytes)?;
    parsed.headers().iter().find_map(|header| {
        let mail_parser::HeaderValue::Text(text) = header.value() else {
            return None;
        };
        header
            .name()
            .eq_ignore_ascii_case(name)
            .then(|| text.trim().to_string())
    })
}

fn assert_gated(bytes: &[u8], verb: &str, prose: &str) {
    let text = String::from_utf8_lossy(bytes);
    assert!(text.contains("X-Vivi-Format: 1"), "{text}");
    assert!(!text.contains("+++"), "empty front must not fence: {text}");
    for moved in [
        "X-Vivi-Verdict",
        "X-Vivi-Repo",
        "X-Vivi-Branch",
        "X-Vivi-Commit",
        "X-Vivi-Tag",
        "X-Vivi-Rule-Scope",
        "X-Vivi-Supersedes",
    ] {
        assert!(!text.contains(moved), "{moved} was written: {text}");
    }
    let letter = of_message(bytes, verb).unwrap_or_else(|err| panic!("{err}: {text}"));
    assert_eq!(letter.kind, LetterKind::Event(verb.to_string()));
    assert_eq!(letter.front, Front::Event(CloseFront::default()));
    assert_eq!(letter.prose, prose);
}

fn marks_of(mailspace: &Mailspace, handle: &str) -> Vec<String> {
    let id = mailspace
        .storage()
        .unwrap()
        .resolve_message_token(handle)
        .unwrap();
    mailspace
        .thread(handle, false, 20, 20)
        .unwrap()
        .into_iter()
        .find(|message| message.message_id == id)
        .unwrap()
        .marks
}

#[test]
fn mark_letters_are_fenceless_format_one_events_and_still_fold() {
    let (_tmp, mailspace) = space();
    let (sent, _) = send_work(&mailspace, "need", "mark me");

    mailspace
        .set_marks("mind", &sent, "need", true, &["ready".into()])
        .unwrap();
    let marked = event_letters(&mailspace, "marked");
    assert!(!marked.is_empty());
    for bytes in &marked {
        assert_gated(bytes, "marked", "marked");
        assert_eq!(header(bytes, "X-Vivi-Event-Slug").as_deref(), Some("ready"));
        let event = super::super::parse_property_event(bytes).unwrap();
        assert_eq!(event.slug.as_deref(), Some("ready"));
    }
    assert_eq!(marks_of(&mailspace, &sent), vec!["ready".to_string()]);

    mailspace
        .set_marks("mind", &sent, "need", false, &["ready".into()])
        .unwrap();
    let unmarked = event_letters(&mailspace, "unmarked");
    assert!(!unmarked.is_empty());
    for bytes in &unmarked {
        assert_gated(bytes, "unmarked", "unmarked");
        assert_eq!(header(bytes, "X-Vivi-Event-Slug").as_deref(), Some("ready"));
    }
    assert!(marks_of(&mailspace, &sent).is_empty());
}

#[test]
fn a_bound_letter_is_a_fenceless_format_one_event_and_still_binds() {
    let (_tmp, mailspace) = space();
    let (_, need) = send_work(&mailspace, "need", "lower me");
    let (_, unit) = send_work(&mailspace, "task", "unit");

    mailspace
        .backlog_bind_units(&need, &[unit.clone()])
        .unwrap();

    let letters = event_letters(&mailspace, "bound");
    assert!(!letters.is_empty());
    for bytes in &letters {
        assert_gated(bytes, "bound", "bound");
        assert!(header(bytes, "X-Vivi-Event-Unit").is_some(), "{bytes:?}");
        let event = super::super::parse_property_event(bytes).unwrap();
        assert!(!event.units.is_empty());
    }
    let show = mailspace.graph_show("backlog").unwrap();
    let need_node = show.nodes.iter().find(|n| n.source_id == need).unwrap();
    let unit_node = show.nodes.iter().find(|n| n.source_id == unit).unwrap();
    assert_eq!(
        unit_node.subgraph.as_deref(),
        Some(need_node.source_id.as_str())
    );
}

#[test]
fn dependency_letters_are_fenceless_format_one_events_and_still_block() {
    let (_tmp, mailspace) = space();
    let (dep, _) = send_work(&mailspace, "need", "work");
    let (release, held) = send_work(&mailspace, "need", "release");

    mailspace
        .set_dependencies("mind", &release, "need", &[dep.clone()], &[])
        .unwrap();
    let added = event_letters(&mailspace, "dependency-added");
    assert!(!added.is_empty());
    for bytes in &added {
        assert_gated(bytes, "dependency-added", "dependency-added");
        assert!(header(bytes, "X-Vivi-Event-Dep").is_some(), "{bytes:?}");
        let event = super::super::parse_property_event(bytes).unwrap();
        assert!(!event.deps.is_empty());
    }
    let readiness = |handle: &str| {
        mailspace
            .graph_show("backlog")
            .unwrap()
            .nodes
            .into_iter()
            .find(|node| node.source_id == handle)
            .unwrap()
            .readiness
    };
    assert_eq!(readiness(&held), "blocked");
    assert_eq!(mailspace.backlog_dependencies(&release).unwrap().len(), 1);

    mailspace
        .set_dependencies("mind", &release, "need", &[], &[dep])
        .unwrap();
    let removed = event_letters(&mailspace, "dependency-removed");
    assert!(!removed.is_empty());
    for bytes in &removed {
        assert_gated(bytes, "dependency-removed", "dependency-removed");
        assert!(header(bytes, "X-Vivi-Event-Dep").is_some());
    }
    assert_eq!(readiness(&held), "ready");
}

#[test]
fn assignment_letters_are_fenceless_format_one_events_and_still_delegate() {
    let (_tmp, mailspace) = space();
    let (_, need) = send_work(&mailspace, "need", "delegate me");

    let assigned = mailspace.assign_need("mind", &need, "other").unwrap();
    let bytes = read(&mailspace, &assigned);
    assert_gated(&bytes, "assigned", "assigned");
    let delegate = header(&bytes, "X-Vivi-Delegate").unwrap();
    assert!(delegate.contains("other@"), "{delegate}");
    let folded = delegate_of(&mailspace, &need);
    assert!(folded.as_deref().unwrap().contains("other@"), "{folded:?}");

    let withdrawn = mailspace.withdraw_need("mind", &need).unwrap();
    let bytes = read(&mailspace, &withdrawn);
    assert_gated(&bytes, "withdrawn", "withdrawn");
    assert!(header(&bytes, "X-Vivi-Delegate").is_none());
    assert!(delegate_of(&mailspace, &need).is_none());
}

fn delegate_of(mailspace: &Mailspace, handle: &str) -> Option<String> {
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(handle).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    held_item(&storage, &view.content_id)
        .unwrap()
        .unwrap()
        .delegation
        .delegate
}

#[test]
fn an_absorbed_letter_is_a_fenceless_format_one_event_and_keeps_its_state() {
    let (_tmp, mut mailspace) = space();
    mailspace.config.absorb = Some(AbsorbConfig { quiet_hours: 0 });
    let (sent, _) = send_work(&mailspace, "need", "seal me");
    mailspace
        .move_item("mind", &sent, "done", None, "need done", None)
        .unwrap();

    mailspace
        .absorb("mind", &sent, Some("sealed the row"), "need")
        .unwrap();

    let letters = event_letters(&mailspace, "absorbed");
    assert!(!letters.is_empty());
    for bytes in &letters {
        assert_gated(bytes, "absorbed", "sealed the row");
        assert_eq!(header(bytes, "X-Vivi-Event-State").as_deref(), Some("done"));
        let event = super::super::parse_property_event(bytes).unwrap();
        assert_eq!(event.state.as_deref(), Some("done"));
    }
    let row = {
        let storage = mailspace.storage().unwrap();
        let id = storage.resolve_message_token(&sent).unwrap();
        storage.message_by_id(&id).unwrap().unwrap()
    };
    assert!(row.absorbed_at.is_some());
}

#[test]
fn a_fenced_property_note_with_an_unknown_key_is_refused() {
    let err = gated_event_body("marked", Some("+++\ncolour = \"red\"\n+++\n\nx")).unwrap_err();
    assert!(err.to_string().contains("unknown field"), "{err}");
}
