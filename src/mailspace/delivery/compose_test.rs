use super::raw_letter;
use crate::letter::fixture::{body_with_deps, need_body, task_body_with};
use crate::letter::records::MailFront;
use crate::letter::work::{NeedFront, TaskFront};
use crate::letter::{Front, LetterKind, of_message};
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

fn nothing_stored(mailspace: &Mailspace) {
    assert!(mailspace.list("hand", "needs").unwrap().is_empty());
    assert!(mailspace.list("hand", "wants").unwrap().is_empty());
    assert!(mailspace.list("hand", "tasks").unwrap().is_empty());
    assert!(mailspace.list("mind", "sent").unwrap().is_empty());
}

#[test]
fn a_prose_need_is_refused_and_nothing_is_stored() {
    let (_tmp, mailspace) = space();
    let err = mailspace
        .send(work("need", "done_when: x\n".into()))
        .unwrap_err()
        .to_string();
    assert!(err.contains("need has no done_when"), "{err}");
    nothing_stored(&mailspace);
}

#[test]
fn a_prose_want_is_refused_and_nothing_is_stored() {
    let (_tmp, mailspace) = space();
    let err = mailspace
        .send(work("want", "what if\n".into()))
        .unwrap_err()
        .to_string();
    assert!(err.contains("want has no question"), "{err}");
    nothing_stored(&mailspace);
}

#[test]
fn a_prose_task_is_refused_and_nothing_is_stored() {
    let (_tmp, mailspace) = space();
    let err = mailspace
        .send(work("task", "do it\n".into()))
        .unwrap_err()
        .to_string();
    assert!(err.contains("task letter has no front matter"), "{err}");
    nothing_stored(&mailspace);
}

fn raw(kind: &str, format: Option<&str>, body: &str) -> Vec<u8> {
    let format = format.map_or(String::new(), |f| format!("X-Vivi-Format: {f}\r\n"));
    format!(
        "From: mind@local\r\nTo: hand@local\r\nSubject: raw\r\nMessage-ID: <raw-{kind}@audit.local>\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\nX-Vivi-Kind: {kind}\r\n{format}\r\n{body}\r\n"
    )
    .into_bytes()
}

#[test]
fn a_raw_legacy_letter_still_imports() {
    let (_tmp, mailspace) = space();
    let data = raw("need", None, "done_when: x\n");
    mailspace.deliver_raw(&data, "inbox").unwrap();
}

#[test]
fn a_raw_format_1_need_with_valid_front_matter_imports() {
    let (_tmp, mailspace) = space();
    let data = raw("need", Some("1"), &need_body(&["done"], "prose"));
    mailspace.deliver_raw(&data, "inbox").unwrap();
}

#[test]
fn a_raw_format_1_need_without_done_when_is_refused_and_nothing_is_stored() {
    let (_tmp, mailspace) = space();
    let body = crate::letter::render(&NeedFront::default(), "prose").unwrap();
    let err = mailspace
        .deliver_raw(&raw("need", Some("1"), &body), "inbox")
        .unwrap_err()
        .to_string();
    assert!(err.contains("letter format:"), "{err}");
    nothing_stored(&mailspace);
    assert!(mailspace.list("hand", "inbox").unwrap().is_empty());
}

#[test]
fn a_raw_letter_with_an_unknown_format_is_refused() {
    let (_tmp, mailspace) = space();
    let err = mailspace
        .deliver_raw(&raw("need", Some("9"), "x"), "inbox")
        .unwrap_err()
        .to_string();
    assert!(err.contains("letter format:"), "{err}");
}

fn mail(kind: Option<&str>, role: &str, body: &str) -> SendRequest {
    SendRequest {
        from: "mind".into(),
        to: vec!["hand".into()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "s".into(),
        body: body.into(),
        role: role.into(),
        kind: kind.map(Into::into),
        reply_to: None,
    }
}

const HAND_ENVELOPE: &str =
    "+++\nanswer = \"built\"\nchange = []\ndeviations = []\nremaining = []\n+++\n\nall done\n";

/// The stored letter with LF line ends.
fn flat(mailspace: &Mailspace, handle: &str) -> String {
    stored(mailspace, handle).replace("\r\n", "\n")
}

fn nothing_mailed(mailspace: &Mailspace) {
    assert!(mailspace.list("hand", "inbox").unwrap().is_empty());
    assert!(mailspace.list("mind", "sent").unwrap().is_empty());
}

/// The body text of a stored letter: everything after the header block.
fn body_of(eml: &str) -> &str {
    eml.split_once("\n\n").map_or("", |(_, body)| body)
}

#[test]
fn a_prose_mail_is_stored_as_written_with_the_format_and_kind_headers() {
    let (_tmp, mailspace) = space();
    for kind in [Some("mail"), None] {
        let result = mailspace.send(mail(kind, "inbox", "hello there")).unwrap();
        let eml = flat(&mailspace, &result.delivered[0].handle);
        assert!(eml.contains("X-Vivi-Format: 1"), "{eml}");
        assert!(eml.contains("X-Vivi-Kind: mail"), "{eml}");
        assert_eq!(body_of(&eml).trim_end(), "hello there", "{eml}");
        assert!(!eml.contains("+++"), "{eml}");
    }
}

#[test]
fn a_prose_reply_is_stored_as_written_and_formatted() {
    let (_tmp, mailspace) = space();
    let first = mailspace
        .send(mail(Some("mail"), "inbox", "question"))
        .unwrap();
    let handle = &first.delivered[0].handle;
    let result = mailspace
        .reply(
            handle,
            "hand",
            Vec::new(),
            Vec::new(),
            None,
            "answer".into(),
        )
        .unwrap();
    let eml = flat(&mailspace, &result.sent);
    assert!(eml.contains("X-Vivi-Format: 1"), "{eml}");
    assert!(eml.contains("X-Vivi-Kind: mail"), "{eml}");
    assert_eq!(body_of(&eml).trim_end(), "answer", "{eml}");
    assert!(!eml.contains("+++"), "{eml}");
}

#[test]
fn a_stored_prose_mail_reads_as_mail_with_an_empty_front() {
    let (_tmp, mailspace) = space();
    let result = mailspace
        .send(mail(Some("mail"), "inbox", "hello there"))
        .unwrap();
    let handle = &result.delivered[0].handle;
    let raw = mailspace.storage().unwrap().read_message(handle).unwrap();
    let letter = of_message(&raw, handle).unwrap();
    assert_eq!(letter.kind, LetterKind::Mail);
    assert_eq!(letter.front, Front::Mail(MailFront::default()));
    assert_eq!(letter.prose, "hello there");
}

#[test]
fn the_raw_gate_admits_a_format_one_prose_mail_and_chat() {
    for kind in ["mail", "chat"] {
        let raw = format!(
            "From: a@b\r\nTo: c@d\r\nSubject: s\r\nX-Vivi-Format: 1\r\nX-Vivi-Kind: {kind}\r\n\r\nhello there\r\n"
        );
        raw_letter(raw.as_bytes()).unwrap();
        let letter = of_message(raw.as_bytes(), "h").unwrap();
        assert_eq!(letter.prose, "hello there");
        assert_eq!(
            matches!(letter.front, Front::Mail(_)),
            kind == "mail",
            "{kind}"
        );
    }
}

#[test]
fn a_stored_wrapped_prose_mail_still_parses() {
    let raw = "From: a@b\r\nTo: c@d\r\nSubject: s\r\nX-Vivi-Format: 1\r\nX-Vivi-Kind: mail\r\n\r\n+++\r\n+++\r\n\r\nold body\r\n";
    raw_letter(raw.as_bytes()).unwrap();
    let letter = of_message(raw.as_bytes(), "h").unwrap();
    assert_eq!(letter.kind, LetterKind::Mail);
    assert_eq!(letter.prose, "old body");
}

#[test]
fn a_mail_front_that_fits_no_front_is_refused_and_nothing_is_stored() {
    let (_tmp, mailspace) = space();
    let err = mailspace
        .send(mail(
            Some("mail"),
            "inbox",
            "+++\ncolour = \"red\"\n+++\n\nhi\n",
        ))
        .unwrap_err()
        .to_string();
    assert!(err.contains("colour"), "{err}");
    assert!(err.contains("not a role reply envelope"), "{err}");
    nothing_mailed(&mailspace);
}

#[test]
fn a_pump_record_with_a_prose_body_is_refused_and_nothing_is_stored() {
    let (_tmp, mailspace) = space();
    let err = mailspace
        .send(mail(Some("pump"), "pumps", "Dispatch of a need."))
        .unwrap_err()
        .to_string();
    assert!(err.contains("front matter fence"), "{err}");
    assert!(mailspace.list("hand", "pumps").unwrap().is_empty());
    assert!(mailspace.list("mind", "sent").unwrap().is_empty());
}

#[test]
fn a_pump_record_with_invalid_front_matter_is_refused() {
    let (_tmp, mailspace) = space();
    let err = mailspace
        .send(mail(Some("pump"), "pumps", "+++\nneed = \"\"\n+++\n\nx\n"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("need"), "{err}");
    assert!(mailspace.list("mind", "sent").unwrap().is_empty());
}

#[test]
fn an_action_front_passes_as_mail() {
    let (_tmp, mailspace) = space();
    let body = "+++\naction = \"withdraw\"\nneed = \"abc\"\n+++\n\nstop\n";
    let result = mailspace.send(mail(Some("mail"), "inbox", body)).unwrap();
    let eml = flat(&mailspace, &result.delivered[0].handle);
    assert!(eml.contains("X-Vivi-Format: 1"), "{eml}");
    assert!(eml.contains("action = \"withdraw\""), "{eml}");
}

#[test]
fn a_hand_envelope_is_accepted_unchanged_by_send_and_by_reply() {
    let (_tmp, mailspace) = space();
    let sent = mailspace
        .send(mail(Some("mail"), "inbox", HAND_ENVELOPE))
        .unwrap();
    let eml = flat(&mailspace, &sent.delivered[0].handle);
    assert!(eml.contains("answer = \"built\"\nchange = []"), "{eml}");
    assert_eq!(eml.matches("+++\n").count(), 2, "{eml}");
    let handle = &sent.delivered[0].handle;
    let reply = mailspace
        .reply(
            handle,
            "hand",
            Vec::new(),
            Vec::new(),
            None,
            HAND_ENVELOPE.into(),
        )
        .unwrap();
    let eml = flat(&mailspace, &reply.sent);
    assert!(eml.contains("X-Vivi-Format: 1"), "{eml}");
    assert!(eml.contains("answer = \"built\"\nchange = []"), "{eml}");
    assert_eq!(eml.matches("+++\n").count(), 2, "{eml}");
}

#[test]
fn chat_is_stored_as_written() {
    let (_tmp, mailspace) = space();
    let result = mailspace.send(mail(Some("chat"), "inbox", "hi")).unwrap();
    let eml = flat(&mailspace, &result.delivered[0].handle);
    assert!(eml.contains("X-Vivi-Format: 1"), "{eml}");
    assert!(eml.contains("X-Vivi-Kind: chat"), "{eml}");
    assert_eq!(body_of(&eml).trim_end(), "hi", "{eml}");
    assert!(!eml.contains("+++"), "{eml}");
}

#[test]
fn a_memo_kind_send_is_unchanged_and_unformatted() {
    let (_tmp, mailspace) = space();
    let result = mailspace
        .send(mail(Some("memo"), "inbox", "plain note"))
        .unwrap();
    let eml = flat(&mailspace, &result.delivered[0].handle);
    assert!(!eml.contains("X-Vivi-Format"), "{eml}");
    assert!(eml.contains("X-Vivi-Kind: memo"), "{eml}");
    assert!(!eml.contains("+++"), "{eml}");
}
