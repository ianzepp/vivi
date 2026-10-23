//! A `bound` letter that arrives before the unit it names binds the unit
//! when the unit's task letter arrives; an older letter never re-targets.

use crate::mailspace::{Mailspace, SendRequest};
use crate::storage::{BACKLOG_GRAPH_CODE, ItemEventWrite, MessageIngestRequest};

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    for role in ["mind", "hand"] {
        mailspace.add_identity(role).unwrap();
    }
    (tmp, mailspace)
}

/// A need sent from `mind` to `hand`: `(handle, Message-ID)`.
fn need(mailspace: &Mailspace, subject: &str) -> (String, String) {
    let result = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: crate::letter::fixture::need_body(&["ok"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap();
    let handle = result
        .delivered
        .first()
        .map_or(result.sent, |delivered| delivered.handle.clone());
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(&handle).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    let held = super::super::held_item(&storage, &view.content_id)
        .unwrap()
        .unwrap();
    (handle, held.rfc_id)
}

/// File a `bound` letter on `need` naming `unit`, dated `date`; returns its
/// content id.
fn file_bound(mailspace: &Mailspace, need: &str, need_rfc: &str, unit: &str, date: &str) -> String {
    let domain = mailspace.address_domain();
    let id = format!("bound-{}-{unit}", date.len() + need.len());
    let bytes = format!(
        "From: mind@{domain}\r\nTo: hand@{domain}\r\nSubject: Re: need\r\n\
         Message-ID: <{id}>\r\nDate: {date}\r\nIn-Reply-To: <{need_rfc}>\r\n\
         X-Vivi-Event: bound\r\nX-Vivi-Event-Of: <{need_rfc}>\r\n\
         X-Vivi-Event-Unit: <{unit}>\r\n\r\nbody\r\n"
    )
    .into_bytes();
    let storage = mailspace.storage().unwrap();
    let parent = storage
        .message_by_id(&storage.resolve_message_token(need).unwrap())
        .unwrap()
        .unwrap();
    let written = storage
        .apply_item_event(&ItemEventWrite {
            reply_requests: &[MessageIngestRequest {
                account: "hand".into(),
                local_role: "inbox".into(),
                read_state: true,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("late-bind\0{id}\0{need}"),
                remote: None,
            }],
            reply_data: Some(&bytes),
            parent_content_id: &parent.content_id,
            moves: &[],
        })
        .unwrap();
    written[0].content_id.clone()
}

/// File an inbound task whose `Message-ID` is `<unit>`, attach it to the
/// backlog, and return its content id.
fn file_unit(mailspace: &Mailspace, unit: &str) -> String {
    let domain = mailspace.address_domain();
    let body = crate::letter::fixture::task_body("work").replace('\n', "\r\n");
    let bytes = format!(
        "From: mind@{domain}\r\nTo: hand@{domain}\r\nSubject: {unit}\r\n\
         Message-ID: <{unit}>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\
         X-Vivi-Kind: task\r\nX-Vivi-Format: 1\r\n\r\n{body}"
    )
    .into_bytes();
    let storage = mailspace.storage().unwrap();
    let stored = storage
        .ingest_message(
            &MessageIngestRequest {
                account: "hand".into(),
                local_role: "tasks".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("late-bind-unit\0{unit}"),
                remote: None,
            },
            &bytes,
        )
        .unwrap();
    drop(storage);
    mailspace
        .backlog_attach_letter("task", &stored.message_id, unit, &bytes)
        .unwrap();
    stored.content_id
}

/// The source ids of the units bound to `need`'s backlog node here.
fn bound_units(mailspace: &Mailspace, need: &str) -> Vec<String> {
    let storage = mailspace.storage().unwrap();
    let graph = storage
        .work_graph_by_code(BACKLOG_GRAPH_CODE)
        .unwrap()
        .expect("the backlog graph exists");
    let mut sources: Vec<String> = storage
        .work_graph_nodes_by_subgraph(&graph.handle, need)
        .unwrap()
        .into_iter()
        .map(|node| node.source_id)
        .collect();
    sources.sort();
    sources
}

const NEWER: &str = "Sun, 11 Oct 2026 12:00:00 +0000";
const OLDER: &str = "Sat, 10 Oct 2026 12:00:00 +0000";

#[test]
fn a_unit_arriving_after_its_bound_letter_binds_to_the_need() {
    let (_tmp, mailspace) = space();
    let (need, need_rfc) = need(&mailspace, "the need");
    let bound = file_bound(&mailspace, &need, &need_rfc, "unit-1@x.test", NEWER);
    mailspace.apply_arrived_events(&[bound]).unwrap();
    assert!(
        bound_units(&mailspace, &need).is_empty(),
        "the unit is not held yet"
    );

    let unit = file_unit(&mailspace, "unit-1@x.test");
    mailspace.apply_arrived_events(&[unit]).unwrap();

    assert_eq!(
        bound_units(&mailspace, &need).len(),
        1,
        "the unit bound once it arrived"
    );
}

#[test]
fn an_older_bound_letter_never_retargets_a_late_unit() {
    let (_tmp, mailspace) = space();
    let (new_home, new_rfc) = need(&mailspace, "the new home");
    let (old_home, old_rfc) = need(&mailspace, "the old home");
    let newer = file_bound(&mailspace, &new_home, &new_rfc, "unit-2@x.test", NEWER);
    let older = file_bound(&mailspace, &old_home, &old_rfc, "unit-2@x.test", OLDER);
    mailspace.apply_arrived_events(&[newer, older]).unwrap();

    let unit = file_unit(&mailspace, "unit-2@x.test");
    mailspace.apply_arrived_events(&[unit]).unwrap();

    assert_eq!(bound_units(&mailspace, &new_home).len(), 1);
    assert!(
        bound_units(&mailspace, &old_home).is_empty(),
        "the older letter moved nothing"
    );
}
