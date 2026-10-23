//! The operator ruling of 2026-10-14, on one mailspace: a role that may
//! close a need, task, or want absorbs it only when its folded state is
//! `done` and its newest held event is at least the quiet period old
//! (`[absorb]` `quiet_hours`, 24 hours by default, `0` off).

use crate::letter::work::TaskFront;
use crate::mailspace::{AbsorbConfig, Mailspace, SendRequest};

/// A mailspace whose `ceo` authors work handed to its `cto`.
fn work_mailspace() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    (tmp, mailspace)
}

/// A task from `ceo` to `cto`; the handle of the author's own `sent` row.
fn send_task(mailspace: &Mailspace) -> String {
    mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "quiet".into(),
            body: crate::letter::fixture::task_body_with(
                &TaskFront {
                    done_when: vec!["it works".into()],
                    ..TaskFront::default()
                },
                "",
            ),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap()
        .sent
}

/// The `X-Vivi-Event-State` of the `absorbed` letter the mailspace holds.
fn absorbed_letter_state(mailspace: &Mailspace) -> Option<String> {
    let storage = mailspace.storage().unwrap();
    for row in storage.list_messages().unwrap() {
        let Ok(bytes) = storage.read_message(&row.message_id) else {
            continue;
        };
        let Some(parsed) = mail_parser::MessageParser::default().parse(&bytes) else {
            continue;
        };
        let mut state = None;
        let mut absorbed = false;
        for header in parsed.headers() {
            let mail_parser::HeaderValue::Text(text) = header.value() else {
                continue;
            };
            if header.name().eq_ignore_ascii_case("X-Vivi-Event") {
                absorbed = text.trim() == "absorbed";
            }
            if header.name().eq_ignore_ascii_case("X-Vivi-Event-State") {
                state = Some(text.trim().to_string());
            }
        }
        if absorbed {
            return state;
        }
    }
    None
}

#[test]
fn an_absorb_of_a_just_closed_item_is_refused_with_the_time_left() {
    let (_tmp, mailspace) = work_mailspace();
    let task = send_task(&mailspace);
    mailspace
        .move_item("ceo", &task, "done", None, "task done", None)
        .unwrap();

    let refused = mailspace
        .absorb("ceo", &task, None, "task")
        .unwrap_err()
        .to_string();

    assert!(
        refused.contains(&format!("{task} is done but not quiet yet: 24h 0m left")),
        "{refused}"
    );
    assert!(refused.contains("newest event "), "{refused}");
    assert!(refused.contains("quiet period 24h"), "{refused}");
}

#[test]
fn an_absorb_of_an_open_item_by_a_closer_is_refused() {
    let (_tmp, mailspace) = work_mailspace();
    let task = send_task(&mailspace);

    let refused = mailspace
        .absorb("ceo", &task, None, "task")
        .unwrap_err()
        .to_string();

    assert!(
        refused.contains(&format!("{task} is open; close it before absorbing")),
        "{refused}"
    );
}

#[test]
fn an_absorb_of_an_open_item_by_a_role_that_cannot_close_still_seals_its_own_copy() {
    let (_tmp, mailspace) = work_mailspace();
    send_task(&mailspace);
    let delivered = mailspace
        .storage()
        .unwrap()
        .list_messages()
        .unwrap()
        .into_iter()
        .find(|row| row.local_role == "tasks")
        .unwrap()
        .handle;

    let absorbed = mailspace.absorb("cto", &delivered, None, "task").unwrap();

    assert_eq!(absorbed, delivered);
    assert!(
        absorbed_letter_state(&mailspace).is_none(),
        "no letter: the holder cannot close the item"
    );
}

#[test]
fn with_quiet_hours_zero_the_absorb_of_a_just_closed_item_succeeds() {
    let (_tmp, mut mailspace) = work_mailspace();
    mailspace.config.absorb = Some(AbsorbConfig { quiet_hours: 0 });
    let task = send_task(&mailspace);
    mailspace
        .move_item("ceo", &task, "done", None, "task done", None)
        .unwrap();

    let absorbed = mailspace.absorb("ceo", &task, None, "task").unwrap();

    assert_eq!(absorbed, task);
    // The letter names the item's folded state, never the folder of the
    // one row the author absorbed: the author's sent copy of a closed
    // item stays `sent`, and the seal still says done.
    assert_eq!(absorbed_letter_state(&mailspace).as_deref(), Some("done"));
}
