use super::*;

const ROOM: &str = "abcde-fghjk-mnpqr-stvwz-23456";
const ID: &str = "msg_0123456789abcdef01234567";
const PARENT: &str = "msg_fedcba9876543210fedcba98";

fn message() -> ChatMessage {
    ChatMessage {
        message_id: ID.into(),
        cursor: 7,
        from: "alpha.checkout-a".into(),
        fingerprint: "0123456789abcdef".into(),
        to: Some("beta.checkout-b".into()),
        subject: "ship the thing".into(),
        body: "line one\nline two\n".into(),
        created_at: "2026-10-09T18:00:00Z".into(),
        kind: Some("need".into()),
        in_reply_to: Some(PARENT.into()),
        refs: vec![PARENT.into()],
    }
}

fn parse(eml: &str) -> mail_parser::Message<'_> {
    mail_parser::MessageParser::default()
        .parse(eml.as_bytes())
        .expect("renders a parseable message")
}

#[test]
fn rendering_is_a_pure_function_of_room_and_record() {
    let first = render_eml(ROOM, &message()).unwrap();
    let second = render_eml(ROOM, &message()).unwrap();
    assert_eq!(first, second);
    // The cursor is a server detail and stays out of the bytes.
    let mut moved = message();
    moved.cursor = 9_000;
    assert_eq!(first, render_eml(ROOM, &moved).unwrap());
}

#[test]
fn rendered_item_parses_with_threading_kind_and_origin() {
    let eml = render_eml(ROOM, &message()).unwrap();
    let parsed = parse(&eml);

    assert_eq!(parsed.subject(), Some("ship the thing"));
    assert_eq!(
        parsed.message_id(),
        Some("msg_0123456789abcdef01234567@abcde-fghjk-mnpqr-stvwz-23456.vivi-room")
    );
    assert!(eml.contains(&format!("In-Reply-To: <{PARENT}@{ROOM}.vivi-room>\n")));
    assert!(eml.contains(&format!("References: <{PARENT}@{ROOM}.vivi-room>\n")));
    assert!(eml.contains("\nX-Vivi-Kind: need\n"));
    assert!(eml.contains("\nX-Vivi-Chat-From: alpha.checkout-a\n"));
    assert!(eml.contains(&format!("\nX-Vivi-Chat-Room: {ROOM}\n")));
    assert!(
        eml.ends_with("\n\nline one\nline two\n"),
        "body is verbatim"
    );
    let from = parsed.from().and_then(|a| a.first()).unwrap();
    assert_eq!(
        from.address(),
        Some(format!("alpha.checkout-a@{ROOM}.vivi-room").as_str())
    );
}

#[test]
fn plain_broadcast_has_no_kind_and_a_room_recipient() {
    let mut plain = message();
    plain.kind = None;
    plain.to = None;
    plain.in_reply_to = None;
    plain.refs.clear();
    let eml = render_eml(ROOM, &plain).unwrap();

    assert!(!eml.contains("X-Vivi-Kind"));
    assert!(!eml.contains("In-Reply-To"));
    assert!(!eml.contains("References"));
    assert!(eml.contains(&format!("\nTo: room@{ROOM}.vivi-room\n")));
}

#[test]
fn hostile_fields_cannot_inject_headers() {
    let mut hostile = message();
    hostile.from = "evil\r\nBcc: victim@example.com".into();
    hostile.fingerprint = "fp\nX-Injected: yes".into();
    hostile.subject = "hi\r\nBcc: victim@example.com".into();
    hostile.to = Some("x>\nBcc: victim@example.com".into());
    hostile.message_id = "id>\r\nBcc: victim@example.com".into();
    let eml = render_eml(ROOM, &hostile).unwrap();
    let (headers, _) = eml.split_once("\n\n").unwrap();

    for line in headers.lines() {
        assert!(!line.starts_with("Bcc:"), "injected header: {line}");
        assert!(!line.starts_with("X-Injected:"), "injected header: {line}");
    }
    assert!(parse(&eml).bcc().is_none());
}

#[test]
fn non_ascii_subject_round_trips_through_an_encoded_word() {
    let mut accented = message();
    accented.subject = "café — déploiement".into();
    let eml = render_eml(ROOM, &accented).unwrap();

    assert!(eml.contains("Subject: =?UTF-8?B?"));
    assert_eq!(parse(&eml).subject(), Some("café — déploiement"));
}

#[test]
fn legacy_ids_are_flattened_not_rejected() {
    let mut legacy = message();
    legacy.message_id = "9f8e7d6c@0123abcd".into();
    let eml = render_eml(ROOM, &legacy).unwrap();

    assert!(eml.contains(&format!(
        "Message-ID: <9f8e7d6c-0123abcd@{ROOM}.vivi-room>\n"
    )));
}

#[test]
fn empty_subject_gets_a_placeholder_and_bad_timestamps_are_rejected() {
    let mut blank = message();
    blank.subject = "  ".into();
    assert_eq!(
        parse(&render_eml(ROOM, &blank).unwrap()).subject(),
        Some("(no subject)")
    );

    let mut bad = message();
    bad.created_at = "yesterday".into();
    let error = render_eml(ROOM, &bad).unwrap_err().to_string();
    assert!(error.contains("invalid created_at"), "{error}");
}

#[test]
fn kinds_file_into_work_folders_and_unknown_kinds_degrade_to_inbox() {
    assert_eq!(role_for(Some("need"), false), "needs");
    assert_eq!(role_for(Some("TASK"), false), "tasks");
    assert_eq!(role_for(Some("want"), false), "wants");
    assert_eq!(role_for(None, false), "inbox");
    assert_eq!(role_for(Some("mail"), false), "inbox");
    assert_eq!(role_for(Some("proposal"), false), "inbox");
    // The seat's own sends are sent copies whatever their kind.
    assert_eq!(role_for(Some("need"), true), "sent");
    assert_eq!(work_kind(Some("proposal")), None);
}
