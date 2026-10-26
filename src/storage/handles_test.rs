use super::*;
use crate::storage::{MessageIngestRequest, StoredMessage};

/// Ingest one message under a fixed `message_id_hint` and return the stored
/// record. Local mirror of the sibling fixture in `storage::tests`, which is
/// not reachable from this module.
fn ingest_hinted(storage: &Storage, message_id: &str, seed: &str) -> StoredMessage {
    storage
        .ingest_message(
            &MessageIngestRequest {
                account: "acct".into(),
                local_role: "inbox".into(),
                read_state: false,
                starred: false,
                message_id_hint: Some(message_id.to_string()),
                presented_id: None,
                seed_hint: seed.into(),
                remote: None,
            },
            &message_bytes(message_id),
        )
        .unwrap()
}

fn message_bytes(message_id: &str) -> Vec<u8> {
    format!(
        "Message-ID: <{message_id}@example.com>\r\n\
         From: Agent <agent@example.com>\r\n\
         To: User <user@example.com>\r\n\
         Subject: hi\r\n\r\nbody {message_id}"
    )
    .into_bytes()
}

#[test]
fn a_cached_reader_resolves_another_connections_committed_message() {
    let tmp = tempfile::tempdir().unwrap();
    let reader = Storage::open_mailspace(tmp.path()).unwrap();
    let writer = Storage::open_mailspace(tmp.path()).unwrap();

    // Fixed ids whose eight-character handles are distinct by construction.
    let first_id = "msg_aaaaaaaa00000000aaaaaaaa0001";
    let second_id = "msg_bbbbbbbb00000000bbbbbbbb0002";
    let first_handle = short_handle(first_id);
    let second_handle = short_handle(second_id);
    assert_eq!(first_handle, "aaaaaaaa");
    assert_eq!(second_handle, "bbbbbbbb");

    let first = ingest_hinted(&reader, first_id, "seed:first");
    assert_eq!(first.message_id, first_id);
    // The fixture content ids are sha256 hex; neither can satisfy the
    // short-token content fallback, so any resolution of these handles below
    // can only come from the handle index.
    assert!(!first.content_id.starts_with(&first_handle));

    // Warm the reader's cache against the pre-commit database.
    assert_eq!(
        reader.resolve_message_token(&first_handle).unwrap(),
        first_id
    );
    assert_eq!(reader.display_handle(first_id).unwrap(), first_handle);

    // Another connection commits a distinct message after the cache warmed.
    let second = ingest_hinted(&writer, second_id, "seed:second");
    assert_eq!(second.message_id, second_id);
    assert!(!second.content_id.starts_with(&second_handle));

    // Without reopening, the original reader resolves the new message's
    // short handle and an unambiguous msg_ id prefix, and reports the new
    // message's correct short display handle.
    assert_eq!(
        reader.resolve_message_token(&second_handle).unwrap(),
        second_id
    );
    assert_eq!(
        reader.resolve_message_token("msg_bbbbbb").unwrap(),
        second_id
    );
    assert_eq!(reader.display_handle(second_id).unwrap(), second_handle);
    // The rebuild kept the pre-commit message resolvable too.
    assert_eq!(
        reader.resolve_message_token(&first_handle).unwrap(),
        first_id
    );
}
