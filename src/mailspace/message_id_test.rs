use crate::mailspace::{Mailspace, SendRequest};
use crate::storage::StoredMessageView;

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

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    for identity in ["mind", "hand"] {
        mailspace.add_identity(identity).unwrap();
    }
    (tmp, mailspace)
}

fn task_request() -> SendRequest {
    SendRequest {
        from: "mind".into(),
        to: vec!["hand".into()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "task one".into(),
        body: "do the thing\n".into(),
        role: "tasks".into(),
        kind: Some("task".into()),
        reply_to: None,
        depends_on: Vec::new(),
    }
}

fn rows(mailspace: &Mailspace) -> Vec<StoredMessageView> {
    mailspace.storage().unwrap().list_messages().unwrap()
}

/// Every letter stored so far sits on the mailspace's own address domain and
/// carries no machine hostname. Copies of one letter share its Message-ID,
/// so distinctness is asserted per letter (content id): distinct letters
/// must carry distinct Message-IDs.
fn assert_letters_belong_to_the_mailspace(mailspace: &Mailspace) {
    let domain = mailspace.address_domain();
    let storage = mailspace.storage().unwrap();
    let mut seen_letters: std::collections::BTreeSet<String> = Default::default();
    let mut seen_ids: std::collections::BTreeSet<String> = Default::default();
    for row in storage.list_messages().unwrap() {
        let bytes = storage.read_message(&row.message_id).unwrap();
        let id = mail_parser::MessageParser::default()
            .parse(&bytes)
            .unwrap()
            .message_id()
            .unwrap()
            .to_string();
        let (_, rhs) = id.split_once('@').unwrap();
        assert_eq!(
            rhs, domain,
            "letter {id} in {} for {} is not on {domain}",
            row.local_role, row.account
        );
        // Copies of one letter share its Message-ID; distinct letters must
        // not.
        if seen_letters.insert(row.content_id.clone()) {
            assert!(
                seen_ids.insert(id.clone()),
                "distinct letters share the Message-ID {id}"
            );
        }
        if let Some(hostname) = machine_hostname() {
            let eml = String::from_utf8_lossy(&bytes);
            assert!(
                !eml.to_ascii_lowercase()
                    .contains(&hostname.to_ascii_lowercase()),
                "letter {id} leaks the machine hostname"
            );
        }
    }
    assert!(!seen_ids.is_empty(), "no letters were stored");
}

/// A path contributes at least `min` letters in `role` for `account`.
fn assert_path_wrote_letters(mailspace: &Mailspace, account: &str, role: &str, min: usize) {
    let count = rows(mailspace)
        .into_iter()
        .filter(|row| row.account == account && row.local_role == role)
        .count();
    assert!(
        count >= min,
        "{account}/{role} has {count} letters, expected at least {min}"
    );
}

#[test]
fn every_compose_path_mints_message_ids_on_the_mailspace_domain() {
    let (_tmp, mailspace) = space();

    // Send: a task send; need and mail sends share the compose path.
    let sent = mailspace.send(task_request()).unwrap();

    // Reply: the assignee answers the delivered copy.
    let parent = sent.delivered[0].handle.clone();
    mailspace
        .reply(
            &parent,
            "hand",
            Vec::new(),
            Vec::new(),
            None,
            "a reply".into(),
        )
        .unwrap();

    // Lifecycle note: the author closes the task with a note letter.
    mailspace
        .move_item(
            "mind",
            &sent.sent,
            "done",
            Some("closing note"),
            "task done",
            None,
        )
        .unwrap();

    // Rule save and memo save: self-addressed letters.
    mailspace
        .save_rule("mind", "keep everything", "keep all mail", false)
        .unwrap();
    mailspace
        .save_memo("mind", "memo subject", "memo body")
        .unwrap();

    assert_path_wrote_letters(&mailspace, "mind", "sent", 2); // send + note
    assert_path_wrote_letters(&mailspace, "hand", "sent", 1); // reply
    assert_path_wrote_letters(&mailspace, "mind", "rules", 1);
    assert_path_wrote_letters(&mailspace, "mind", "memos", 1);
    assert_letters_belong_to_the_mailspace(&mailspace);
}
