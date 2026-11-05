use super::*;

/// The machine's hostname, for asserting composed letters never carry it:
/// the first line of `/proc/sys/kernel/hostname`, falling back to
/// `/etc/hostname`, then the `HOSTNAME` environment variable.
fn machine_hostname() -> Option<String> {
    for path in ["/proc/sys/kernel/hostname", "/etc/hostname"] {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        if let Some(line) = content.lines().next() {
            let line = line.trim();
            if !line.is_empty() {
                return Some(line.to_string());
            }
        }
    }
    std::env::var("HOSTNAME")
        .ok()
        .filter(|hostname| !hostname.is_empty())
}

fn draft(from: &str) -> ComposeDraft {
    ComposeDraft {
        from: from.into(),
        to: vec!["peer@example.local".into()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "subject".into(),
        body: "body".into(),
        html_body: None,
    }
}

fn message_id_of(eml: &str) -> String {
    mail_parser::MessageParser::default()
        .parse(eml.as_bytes())
        .unwrap()
        .message_id()
        .unwrap()
        .to_string()
}

fn assert_id_domain(eml: &str, domain: &str) {
    let id = message_id_of(eml);
    let (_, rhs) = id.split_once('@').unwrap();
    assert_eq!(rhs, domain, "Message-ID {id} is not on {domain}");
}

/// Skip only this assertion when no hostname is readable.
fn assert_no_hostname(eml: &str) {
    let Some(hostname) = machine_hostname() else {
        return;
    };
    assert!(
        !eml.to_ascii_lowercase()
            .contains(&hostname.to_ascii_lowercase()),
        "composed letter leaks the machine hostname"
    );
}

fn parsed_of(eml: &str) -> mail_parser::Message<'_> {
    mail_parser::MessageParser::default()
        .parse(eml.as_bytes())
        .unwrap()
}

fn without_recipients(mut draft: ComposeDraft) -> ComposeDraft {
    draft.to = Vec::new();
    draft.cc = Vec::new();
    draft.bcc = Vec::new();
    draft
}

fn has_recipients(parsed: &mail_parser::Message<'_>) -> bool {
    parsed.to().and_then(|a| a.first()).is_some()
        || parsed.cc().and_then(|a| a.first()).is_some()
        || parsed.bcc().and_then(|a| a.first()).is_some()
}

#[test]
fn recipientless_draft_has_from_and_no_recipient_headers() {
    let recipientless = without_recipients(draft("mind@abcd1234.local"));
    let eml = build_compose_draft(&recipientless).unwrap();
    let parsed = parsed_of(&eml);
    assert!(parsed.from().and_then(|a| a.first()).is_some());
    assert!(
        !has_recipients(&parsed),
        "recipientless letter grew recipients"
    );
}

#[test]
fn recipientless_draft_with_attachments_still_composes() {
    let recipientless = without_recipients(draft("mind@abcd1234.local"));
    let attachments = vec![FileAttachment {
        filename: "notes.txt".into(),
        content_type: "text/plain".into(),
        data: b"attached bytes".to_vec(),
    }];
    let eml = build_compose_draft_with_attachments(&recipientless, &attachments).unwrap();
    let parsed = parsed_of(&eml);
    assert!(parsed.from().and_then(|a| a.first()).is_some());
    assert!(
        !has_recipients(&parsed),
        "recipientless letter grew recipients"
    );
    assert_eq!(parsed.attachment_count(), 1, "attachment was dropped");
}

#[test]
fn cc_only_draft_composes_without_to_or_bcc() {
    let mut cc_only = draft("mind@abcd1234.local");
    cc_only.to = Vec::new();
    cc_only.cc = vec!["watcher@example.local".into()];
    let eml = build_compose_draft(&cc_only).unwrap();
    let parsed = parsed_of(&eml);
    assert_eq!(
        parsed.cc().and_then(|a| a.first()).unwrap().address(),
        Some("watcher@example.local")
    );
    assert!(parsed.to().and_then(|a| a.first()).is_none());
    assert!(parsed.bcc().and_then(|a| a.first()).is_none());
}

#[test]
fn missing_from_still_refused() {
    let no_from = without_recipients(draft(""));
    assert!(build_compose_draft(&no_from).is_err());
    assert!(build_compose_draft_with_attachments(&no_from, &[]).is_err());
    let err = validate_message_headers(b"Subject: none\r\n\r\nbody\r\n")
        .expect_err("a message without From must be refused");
    assert!(err.to_string().contains("From"), "wrong refusal: {err}");
}

#[test]
fn message_id_right_hand_side_is_the_from_domain() {
    let eml = build_compose_draft(&draft("mind@abcd1234.local")).unwrap();
    assert_id_domain(&eml, "abcd1234.local");
    assert_no_hostname(&eml);
}

#[test]
fn message_id_falls_back_to_vivi_local_without_a_from_domain() {
    let eml = build_compose_draft(&draft("mind")).unwrap();
    assert_id_domain(&eml, "vivi.local");
    assert_no_hostname(&eml);
}

#[test]
fn reply_message_id_right_hand_side_is_the_from_domain() {
    let original = build_compose_draft(&draft("peer@example.local")).unwrap();
    let eml = build_reply(
        original.as_bytes(),
        &ReplyDraft {
            from: "mind@abcd1234.local".into(),
            body: "a reply".into(),
            html_body: None,
        },
    )
    .unwrap();
    assert_id_domain(&eml, "abcd1234.local");
    assert_no_hostname(&eml);
}

#[test]
fn two_letters_composed_back_to_back_get_distinct_message_ids() {
    let first = message_id_of(&build_compose_draft(&draft("mind@abcd1234.local")).unwrap());
    let second = message_id_of(&build_compose_draft(&draft("mind@abcd1234.local")).unwrap());
    assert_ne!(first, second, "back-to-back letters share a Message-ID");
}
