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

#[test]
fn a_read_only_cached_reader_observes_an_external_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let writer = Storage::open_mailspace(tmp.path()).unwrap();

    let first_id = "msg_1111111100000000111111110001";
    let second_id = "msg_2222222200000000222222220002";
    let first_handle = short_handle(first_id);
    let second_handle = short_handle(second_id);
    assert_eq!(first_handle, "11111111");
    assert_eq!(second_handle, "22222222");

    // A committed message exists before the reader opens, so its first
    // index build has something to hold.
    let first = ingest_hinted(&writer, first_id, "seed:first");
    assert!(!first.content_id.starts_with(&first_handle));

    // The writer stays open below, so its WAL/-shm sidecars exist and the
    // guarded read-only open is allowed to proceed.
    assert!(!Storage::read_only_wal_index_missing(tmp.path()));
    let reader = Storage::open_mailspace_read_only(tmp.path()).unwrap();

    // Warm the reader's cached index against the pre-commit database.
    assert_eq!(
        reader.resolve_message_token(&first_handle).unwrap(),
        first_id
    );

    // The still-open writer connection commits a distinct message.
    let second = ingest_hinted(&writer, second_id, "seed:second");
    assert!(!second.content_id.starts_with(&second_handle));

    // Without reopening, the read-only reader observes the external commit.
    assert_eq!(
        reader.resolve_message_token(&second_handle).unwrap(),
        second_id
    );
    assert_eq!(reader.display_handle(second_id).unwrap(), second_handle);
    assert_eq!(
        reader.resolve_message_token(&first_handle).unwrap(),
        first_id
    );
}

#[test]
fn a_cached_success_becomes_ambiguous_after_an_external_insert() {
    let tmp = tempfile::tempdir().unwrap();
    let reader = Storage::open_mailspace(tmp.path()).unwrap();
    let writer = Storage::open_mailspace(tmp.path()).unwrap();

    let first_id = "msg_abcd1234aaaabbbbcccc0001";
    let second_id = "msg_abcd1234aaaabbbbcccc0002";
    assert_eq!(short_handle(first_id), "abcd1234");
    assert_eq!(short_handle(second_id), "abcd1234");

    let first = ingest_hinted(&reader, first_id, "seed:first");
    assert!(!first.content_id.starts_with("abcd1234"));

    // Warm a successful resolution through the cache.
    assert_eq!(reader.resolve_message_token("abcd1234").unwrap(), first_id);

    // Another connection inserts a second message whose id shares the same
    // first eight basis characters.
    let second = ingest_hinted(&writer, second_id, "seed:second");
    assert!(!second.content_id.starts_with("abcd1234"));

    // The warmed reader now refuses the shared short token (error presence
    // only; the prose is not asserted), while both complete ids resolve.
    assert!(reader.resolve_message_token("abcd1234").is_err());
    assert_eq!(reader.resolve_message_token(first_id).unwrap(), first_id);
    assert_eq!(reader.resolve_message_token(second_id).unwrap(), second_id);
}

#[test]
fn an_external_delete_invalidates_a_cached_handle() {
    let tmp = tempfile::tempdir().unwrap();
    let reader = Storage::open_mailspace(tmp.path()).unwrap();
    let writer = Storage::open_mailspace(tmp.path()).unwrap();

    let id = "msg_cccccccc00000000cccccccc0003";
    let handle = short_handle(id);
    assert_eq!(handle, "cccccccc");
    let stored = ingest_hinted(&reader, id, "seed:third");
    assert!(!stored.content_id.starts_with(&handle));

    // Warm the reader's cache, then soft-delete through the other
    // connection.
    assert_eq!(reader.resolve_message_token(&handle).unwrap(), id);
    assert!(writer.mark_message_deleted("acct", id).unwrap());

    // The cached reader no longer resolves the deleted handle and no longer
    // decorates the full id with its short handle.
    assert!(reader.resolve_message_token(&handle).is_err());
    assert_eq!(reader.display_handle(id).unwrap(), id);
}

#[test]
fn local_ingest_and_rollback_invalidate_the_handle_index() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::open_mailspace(tmp.path()).unwrap();

    let kept_id = "msg_dddddddd00000000dddddddd0004";
    let kept_handle = short_handle(kept_id);
    let kept = ingest_hinted(&storage, kept_id, "seed:kept");
    assert!(!kept.content_id.starts_with(&kept_handle));

    // Warm the index, then ingest again and prove immediate visibility.
    assert_eq!(
        storage.resolve_message_token(&kept_handle).unwrap(),
        kept_id
    );
    let ingested_id = "msg_eeeeeeee00000000eeeeeeee0005";
    let ingested = ingest_hinted(&storage, ingested_id, "seed:ingested");
    let ingested_handle = short_handle(ingested_id);
    assert!(!ingested.content_id.starts_with(&ingested_handle));
    assert_eq!(
        storage.resolve_message_token(&ingested_handle).unwrap(),
        ingested_id
    );

    // Inside a batch, the uncommitted row resolves through the index.
    let batched_id = "msg_ffffffff00000000ffffffff0006";
    let batched_handle = short_handle(batched_id);
    storage.begin_batch().unwrap();
    let batched = ingest_hinted(&storage, batched_id, "seed:batched");
    assert!(!batched.content_id.starts_with(&batched_handle));
    assert_eq!(
        storage.resolve_message_token(&batched_handle).unwrap(),
        batched_id
    );

    // The rollback drops the row and the index returns to committed state.
    storage.rollback_batch().unwrap();
    assert!(storage.resolve_message_token(&batched_handle).is_err());
    assert!(
        storage
            .message_by_id(&batched.message_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        storage.resolve_message_token(&kept_handle).unwrap(),
        kept_id
    );
    assert_eq!(
        storage.resolve_message_token(&ingested_handle).unwrap(),
        ingested_id
    );
}

#[test]
fn unchanged_reads_reuse_the_handle_index() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::open_mailspace(tmp.path()).unwrap();

    let id = "msg_1234567800000000123456780007";
    let handle = short_handle(id);
    let stored = ingest_hinted(&storage, id, "seed:warm");
    assert!(!stored.content_id.starts_with(&handle));

    // Warm the cache, then hold one index Ref while obtaining another: a
    // rebuild here would need a mutable borrow of the occupied RefCell and
    // panic, and pointer identity shows no replacement happened.
    assert_eq!(storage.resolve_message_token(&handle).unwrap(), id);
    let held = storage.handle_index().unwrap();
    let again = storage.handle_index().unwrap();
    assert!(std::ptr::eq(&*held, &*again));

    // Ordinary lookup and display calls reuse the held index too.
    assert_eq!(storage.resolve_message_token(&handle).unwrap(), id);
    assert_eq!(storage.display_handle(id).unwrap(), handle);
    drop(held);
}
