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
