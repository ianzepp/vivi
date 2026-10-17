use super::*;

#[test]
fn schema_upgrade_creates_mid_version_tables_on_old_databases() {
    // Simulate a mailspace created at schema 6 before the goals and
    // work-graph tables existed: metadata row only, no new tables.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("mail.sqlite");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE storage_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO storage_metadata (key, value) VALUES ('schema_version', '6');",
    )
    .unwrap();
    schema::ensure_schema(&conn).unwrap();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    for table in ["work_graphs", "work_graph_nodes", "mailspace_goals"] {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                rusqlite::params![table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "table {table} missing after upgrade");
    }
    let version: String = conn
        .query_row(
            "SELECT value FROM storage_metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, "8");
}

#[test]
fn schema_v7_adds_node_kind_and_edge_style_on_upgrade() {
    // Simulate a schema-7 mailspace whose graph tables predate node kinds
    // and edge styles: one node and one edge without the new columns.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("mail.sqlite");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE storage_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO storage_metadata (key, value) VALUES ('schema_version', '7');
         CREATE TABLE work_graph_nodes (
           handle TEXT PRIMARY KEY,
           graph_handle TEXT NOT NULL,
           source_id TEXT NOT NULL,
           label TEXT NOT NULL,
           state TEXT NOT NULL DEFAULT 'open',
           subgraph TEXT,
           created_at TEXT NOT NULL,
           updated_at TEXT NOT NULL,
           UNIQUE (graph_handle, source_id)
         );
         CREATE TABLE work_graph_edges (
           handle TEXT PRIMARY KEY,
           graph_handle TEXT NOT NULL,
           from_node TEXT NOT NULL,
           to_node TEXT NOT NULL,
           label TEXT,
           created_at TEXT NOT NULL,
           UNIQUE (graph_handle, from_node, to_node)
         );
         INSERT INTO work_graph_nodes
           (handle, graph_handle, source_id, label, state, created_at, updated_at)
           VALUES ('nod_x', 'gph_x', 'res_provision', 'Provision', 'open', 't', 't');
         INSERT INTO work_graph_edges
           (handle, graph_handle, from_node, to_node, label, created_at)
           VALUES ('edg_x', 'gph_x', 'nod_a', 'nod_x', NULL, 't');",
    )
    .unwrap();
    drop(conn);
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    schema::ensure_schema(&conn).unwrap();

    let kind: String = conn
        .query_row(
            "SELECT kind FROM work_graph_nodes WHERE handle = 'nod_x'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kind, "task");
    let style: String = conn
        .query_row(
            "SELECT style FROM work_graph_edges WHERE handle = 'edg_x'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(style, "solid");
    let version: String = conn
        .query_row(
            "SELECT value FROM storage_metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, "8");
}

#[test]
fn ingest_dedupes_blobs_but_keeps_distinct_message_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let raw = message_bytes("dup@example.com", "same body");
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();

    let first = storage
        .ingest_message(&bound_request("one", "INBOX", 7), &raw)
        .unwrap();
    let second = storage
        .ingest_message(&bound_request("two", "Archive", 8), &raw)
        .unwrap();

    assert_ne!(first.message_id, second.message_id);
    assert!(first.created_blob);
    assert!(!second.created_blob);
    assert_eq!(storage.blob_count().unwrap(), 1);
    assert_eq!(storage.message_count().unwrap(), 2);
    assert_eq!(storage.remote_binding_count().unwrap(), 2);
}

#[test]
fn ingest_persists_blob_and_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let raw = b"Message-ID: <meta@example.com>\r\nFrom: Agent <agent@example.com>\r\nTo: User <user@example.com>\r\nSubject: hello\r\n\r\nbody";
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();

    storage
        .ingest_message(&bound_request("one", "INBOX", 7), raw)
        .unwrap();
    let data = storage.read_blob(&resulting_content_id(raw)).unwrap();

    assert_eq!(data, raw);
    assert_eq!(storage.blob_count().unwrap(), 1);
    assert_eq!(storage.message_count().unwrap(), 1);
}

#[test]
fn fallback_message_ids_are_stable_for_unbound_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let raw = message_bytes("local@example.com", "body");
    let request = MessageIngestRequest {
        account: "acct".into(),
        local_role: local_role("Drafts"),
        read_state: false,
        starred: false,
        message_id_hint: Some("draft-handle".into()),
        presented_id: None,
        seed_hint: "draft-handle".into(),
        remote: None,
    };

    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let first = storage.ingest_message(&request, &raw).unwrap();
    let second = storage.ingest_message(&request, &raw).unwrap();

    assert_eq!(first.message_id, second.message_id);
    assert_eq!(storage.message_count().unwrap(), 1);
}

#[test]
fn direct_ingest_api_supports_clean_break_sync_target() {
    let tmp = tempfile::tempdir().unwrap();
    let raw = message_bytes("direct@example.com", "body");
    let request = MessageIngestRequest {
        account: "acct".into(),
        local_role: "inbox".into(),
        read_state: false,
        starred: false,
        message_id_hint: None,
        presented_id: None,
        seed_hint: "uid:99".into(),
        remote: Some(RemoteBindingInput {
            account: "acct".into(),
            provider: "protonmail".into(),
            remote_mailbox: "INBOX".into(),
            remote_uid: 99,
            remote_uidvalidity: 42,
        }),
    };

    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let stored = storage.ingest_message(&request, &raw).unwrap();

    assert!(stored.message_id.starts_with("msg_"));
    assert_eq!(storage.blob_count().unwrap(), 1);
    assert_eq!(storage.message_count().unwrap(), 1);
    assert_eq!(storage.remote_binding_count().unwrap(), 1);
}

#[test]
fn short_handles_resolve_uniquely_for_storage_native_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();

    let first = storage
        .ingest_message(
            &MessageIngestRequest {
                account: "acct".into(),
                local_role: "inbox".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: "remote_uid:1".into(),
                remote: Some(RemoteBindingInput {
                    account: "acct".into(),
                    provider: "protonmail".into(),
                    remote_mailbox: "INBOX".into(),
                    remote_uid: 1,
                    remote_uidvalidity: 42,
                }),
            },
            &message_bytes("one@example.com", "first"),
        )
        .unwrap();
    let second = storage
        .ingest_message(
            &MessageIngestRequest {
                account: "acct".into(),
                local_role: "inbox".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: "remote_uid:2".into(),
                remote: Some(RemoteBindingInput {
                    account: "acct".into(),
                    provider: "protonmail".into(),
                    remote_mailbox: "INBOX".into(),
                    remote_uid: 2,
                    remote_uidvalidity: 42,
                }),
            },
            &message_bytes("two@example.com", "second"),
        )
        .unwrap();

    let first_handle = storage.display_handle(&first.message_id).unwrap();
    let second_handle = storage.display_handle(&second.message_id).unwrap();

    assert_ne!(first_handle, second_handle);
    assert!(first_handle.len() >= 7);
    assert_eq!(
        storage.resolve_message_token(&first_handle).unwrap(),
        first.message_id
    );
    assert_eq!(
        storage.resolve_message_token(&second_handle).unwrap(),
        second.message_id
    );
}

#[test]
fn short_handle_map_is_a_fixed_width_prefix_of_the_basis() {
    let ids = vec![
        "msg_abcd1234aaaabbbbcccc0001".to_string(),
        "msg_abcd1234aaaabbbbcccc0002".to_string(),
        "msg_beef5678aaaabbbbcccc0003".to_string(),
        "not-a-native-id".to_string(),
    ];

    let map = short_handle_map(&ids);

    assert_eq!(map["msg_abcd1234aaaabbbbcccc0001"], "abcd1234");
    assert_eq!(map["msg_beef5678aaaabbbbcccc0003"], "beef5678");
    // A shared prefix is a shared handle: the map does no cross-record work.
    assert_eq!(
        map["msg_abcd1234aaaabbbbcccc0001"],
        map["msg_abcd1234aaaabbbbcccc0002"]
    );
    // An id that is not storage-native is its own handle.
    assert_eq!(map["not-a-native-id"], "not-a-native-id");
}

#[test]
fn a_shared_short_handle_resolves_as_ambiguous_rather_than_by_guess() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let first = ingest_hinted(&mut storage, "msg_abcd1234aaaabbbbcccc0001", 1);
    let second = ingest_hinted(&mut storage, "msg_abcd1234aaaabbbbcccc0002", 2);
    assert_ne!(first, second);

    assert_eq!(storage.display_handle(&first).unwrap(), "abcd1234");
    assert_eq!(storage.display_handle(&second).unwrap(), "abcd1234");
    // The shared token is reported, never resolved by guessing.
    assert!(storage.resolve_message_token("abcd1234").is_err());
    // The full id still addresses each message.
    assert_eq!(storage.resolve_message_token(&first).unwrap(), first);
    assert_eq!(storage.resolve_message_token(&second).unwrap(), second);
}

fn ingest_hinted(storage: &mut Storage, message_id: &str, remote_uid: u32) -> String {
    storage
        .ingest_message(
            &MessageIngestRequest {
                account: "acct".into(),
                local_role: "inbox".into(),
                read_state: false,
                starred: false,
                message_id_hint: Some(message_id.to_string()),
                presented_id: None,
                seed_hint: format!("remote_uid:{remote_uid}"),
                remote: Some(RemoteBindingInput {
                    account: "acct".into(),
                    provider: "protonmail".into(),
                    remote_mailbox: "INBOX".into(),
                    remote_uid,
                    remote_uidvalidity: 42,
                }),
            },
            &message_bytes("hinted@example.com", "body"),
        )
        .unwrap()
        .message_id
}

#[test]
fn content_id_prefix_can_resolve_message() {
    let tmp = tempfile::tempdir().unwrap();
    let raw = message_bytes("content@example.com", "body");
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let stored = storage
        .ingest_message(
            &MessageIngestRequest {
                account: "acct".into(),
                local_role: "inbox".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: "remote_uid:3".into(),
                remote: Some(RemoteBindingInput {
                    account: "acct".into(),
                    provider: "protonmail".into(),
                    remote_mailbox: "INBOX".into(),
                    remote_uid: 3,
                    remote_uidvalidity: 42,
                }),
            },
            &raw,
        )
        .unwrap();

    let prefix = &stored.content_id[..12];
    assert_eq!(
        storage.resolve_message_token(prefix).unwrap(),
        stored.message_id
    );
}

#[test]
fn local_size_fallback_uses_remote_uid_shape_for_storage_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let raw = message_bytes("size@example.com", "body");
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    storage
        .ingest_message(
            &MessageIngestRequest {
                account: "acct".into(),
                local_role: "inbox".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: "remote_uid:7".into(),
                remote: Some(RemoteBindingInput {
                    account: "acct".into(),
                    provider: "protonmail".into(),
                    remote_mailbox: "INBOX".into(),
                    remote_uid: 7,
                    remote_uidvalidity: 42,
                }),
            },
            &raw,
        )
        .unwrap();

    let sizes = storage.local_sizes_by_role("inbox").unwrap();
    assert_eq!(sizes.get("inbox-7"), Some(&(raw.len() as u64)));
}

#[test]
fn latest_from_prefers_exact_address_and_ignores_memos() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    ingest_dated(
        &mut storage,
        "mind",
        "sent",
        "mind@faberlang.local",
        "Wed, 01 Jan 2020 00:00:00 +0000",
        "old-mail",
    );
    ingest_dated(
        &mut storage,
        "mind",
        "memos",
        "mind@faberlang.local",
        "Fri, 03 Jan 2020 00:00:00 +0000",
        "newer-memo",
    );
    let newer = ingest_dated(
        &mut storage,
        "mind",
        "sent",
        "mind@faberlang.local",
        "Thu, 02 Jan 2020 00:00:00 +0000",
        "newer-mail",
    );
    let found = storage
        .latest_message_from_addresses(&["mind@faberlang.local".into()])
        .unwrap()
        .expect("signal");
    assert_eq!(found.message_id, newer);
    assert_eq!(found.local_role, "sent");
    assert!(!found.handle.is_empty());
}

#[test]
fn latest_from_matches_display_name_form() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let id = ingest_dated(
        &mut storage,
        "acct",
        "sent",
        "Agent <agent@example.com>",
        "Thu, 02 Jan 2020 00:00:00 +0000",
        "named",
    );
    let found = storage
        .latest_message_from_addresses(&["agent@example.com".into()])
        .unwrap()
        .expect("display-name from");
    assert_eq!(found.message_id, id);
}

#[test]
fn schema_v6_adds_from_addr_date_index_on_upgrade() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::open_mailspace(tmp.path()).unwrap();
    storage
        .conn
        .execute("DROP INDEX message_metadata_from_addr_date_idx", [])
        .unwrap();
    storage
        .conn
        .execute(
            "INSERT OR REPLACE INTO storage_metadata (key, value) VALUES ('schema_version', '5')",
            [],
        )
        .unwrap();
    drop(storage);
    let storage = Storage::open_mailspace(tmp.path()).unwrap();
    let present: i64 = storage
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'message_metadata_from_addr_date_idx'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(present, 1);
    let version: String = storage
        .conn
        .query_row(
            "SELECT value FROM storage_metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, "8");
}

fn ingest_dated(
    storage: &mut Storage,
    account: &str,
    role: &str,
    from: &str,
    date: &str,
    seed: &str,
) -> String {
    let raw = format!(
        "Message-ID: <{seed}@example.com>\r\nFrom: {from}\r\nTo: x@example.com\r\n\
         Date: {date}\r\nSubject: s\r\n\r\nbody"
    )
    .into_bytes();
    storage
        .ingest_message(
            &MessageIngestRequest {
                account: account.into(),
                local_role: role.into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: seed.into(),
                remote: None,
            },
            &raw,
        )
        .unwrap()
        .message_id
}

fn message_bytes(message_id: &str, body: &str) -> Vec<u8> {
    format!(
            "Message-ID: <{message_id}>\r\nFrom: Agent <agent@example.com>\r\nTo: User <user@example.com>\r\nSubject: hi\r\n\r\n{body}"
        )
        .into_bytes()
}

fn resulting_content_id(data: &[u8]) -> String {
    sha256_hex(data)
}

fn bound_request(handle: &str, mailbox: &str, uid: u32) -> MessageIngestRequest {
    MessageIngestRequest {
        account: "acct".into(),
        local_role: local_role(mailbox),
        read_state: false,
        starred: false,
        message_id_hint: Some(handle.into()),
        presented_id: None,
        seed_hint: handle.into(),
        remote: Some(RemoteBindingInput {
            account: "acct".into(),
            provider: "protonmail".into(),
            remote_mailbox: mailbox.into(),
            remote_uid: uid,
            remote_uidvalidity: 42,
        }),
    }
}

#[test]
fn move_message_to_role_rejects_wrong_account() {
    let tmp = tempfile::tempdir().unwrap();
    let raw = message_bytes("move@example.com", "body");
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let stored = storage
        .ingest_message(
            &MessageIngestRequest {
                account: "acct".into(),
                local_role: "inbox".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: "remote_uid:5".into(),
                remote: Some(RemoteBindingInput {
                    account: "acct".into(),
                    provider: "protonmail".into(),
                    remote_mailbox: "INBOX".into(),
                    remote_uid: 5,
                    remote_uidvalidity: 42,
                }),
            },
            &raw,
        )
        .unwrap();

    let err = storage
        .move_message_to_role("other", &stored.message_id, "archive")
        .unwrap_err();
    assert!(err.to_string().contains("not found"));

    // Original message must be unchanged.
    let sizes = storage.local_sizes_by_role("inbox").unwrap();
    assert!(sizes.contains_key("inbox-5"));
}

#[test]
fn mark_message_deleted_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let raw = message_bytes("del@example.com", "body");
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let stored = storage
        .ingest_message(
            &MessageIngestRequest {
                account: "acct".into(),
                local_role: "inbox".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: "remote_uid:6".into(),
                remote: Some(RemoteBindingInput {
                    account: "acct".into(),
                    provider: "protonmail".into(),
                    remote_mailbox: "INBOX".into(),
                    remote_uid: 6,
                    remote_uidvalidity: 42,
                }),
            },
            &raw,
        )
        .unwrap();

    // First delete succeeds.
    assert!(
        storage
            .mark_message_deleted("acct", &stored.message_id)
            .unwrap()
    );

    // Second delete returns false — already soft-deleted.
    assert!(
        !storage
            .mark_message_deleted("acct", &stored.message_id)
            .unwrap()
    );
}

// ---------------------------------------------------------------------------
// Test helpers extracted from production modules
// ---------------------------------------------------------------------------

fn local_role(folder: &str) -> String {
    match folder {
        "INBOX" | "Inbox" | "inbox" => "inbox".into(),
        "Archive" | "archive" => "archive".into(),
        "Trash" | "trash" => "trash".into(),
        "Sent" | "sent" => "sent".into(),
        "Drafts" | "drafts" => "drafts".into(),
        other => other.to_ascii_lowercase(),
    }
}

impl Storage {
    fn blob_count(&self) -> Result<usize, VivariumError> {
        self.conn
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get(0))
            .map_err(|e| VivariumError::Other(format!("failed to count blobs: {e}")))
    }

    fn message_count(&self) -> Result<usize, VivariumError> {
        self.conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .map_err(|e| VivariumError::Other(format!("failed to count messages: {e}")))
    }

    fn remote_binding_count(&self) -> Result<usize, VivariumError> {
        self.conn
            .query_row("SELECT COUNT(*) FROM remote_bindings", [], |row| row.get(0))
            .map_err(|e| VivariumError::Other(format!("failed to count remote bindings: {e}")))
    }
}

const TRAVELING_ID: &str = "msg_0123456789abcdef01234567";

fn request_in(
    account: &str,
    role: &str,
    seed_hint: &str,
    presented: Option<&str>,
    remote: Option<RemoteBindingInput>,
) -> MessageIngestRequest {
    MessageIngestRequest {
        account: account.into(),
        local_role: role.into(),
        read_state: false,
        starred: false,
        message_id_hint: None,
        presented_id: presented.map(String::from),
        seed_hint: seed_hint.into(),
        remote,
    }
}

fn letter_binding(cursor: u32) -> RemoteBindingInput {
    RemoteBindingInput {
        account: "acct-b".into(),
        provider: "vivi-project".into(),
        remote_mailbox: "project-x".into(),
        remote_uid: cursor,
        remote_uidvalidity: 1,
    }
}

#[test]
fn traveling_id_shape_is_strict() {
    assert!(is_traveling_id(TRAVELING_ID));
    for bad in [
        "",
        "msg_",
        "msg_0123456789abcdef0123456",   // 23 hex
        "msg_0123456789abcdef012345678", // 25 hex
        "msg_0123456789ABCDEF01234567",  // uppercase
        "msg_0123456789abcdef0123456g",  // non-hex
        "0123456789abcdef01234567",      // no prefix
        " msg_0123456789abcdef01234567",
        "msg_0123456789abcdef01234567\n",
        "draft-handle",
    ] {
        assert!(!is_traveling_id(bad), "{bad:?} must not be a traveling id");
    }
}

#[test]
fn presented_id_wins_over_seeded_mints_and_agrees_across_stores() {
    let raw = message_bytes("travel@example.com", "a need");
    let content_id = sha256_hex(&raw);
    let local = request_in("acct-a", "inbox", "seed-a", None, None);
    let bound = request_in("acct-b", "needs", "seed-b", None, Some(letter_binding(5)));
    // The failure this exists to fix: namespace seeds give two ids.
    let seeded_local = ingest::ingest_message_id(&local, &content_id).unwrap();
    let seeded_bound = ingest::ingest_message_id(&bound, &content_id).unwrap();
    assert_ne!(seeded_local, seeded_bound);

    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();
    let mut store_a = Storage::open_mailspace(tmp_a.path()).unwrap();
    let mut store_b = Storage::open_mailspace(tmp_b.path()).unwrap();
    let id_a = store_a
        .ingest_message(
            &request_in("acct-a", "inbox", "seed-a", Some(TRAVELING_ID), None),
            &raw,
        )
        .unwrap()
        .message_id;
    let id_b = store_b
        .ingest_message(
            &request_in(
                "acct-b",
                "needs",
                "seed-b",
                Some(TRAVELING_ID),
                Some(letter_binding(5)),
            ),
            &raw,
        )
        .unwrap()
        .message_id;

    assert_eq!(id_a, TRAVELING_ID);
    assert_eq!(id_b, TRAVELING_ID);
    assert_eq!(store_b.remote_binding_count().unwrap(), 1);
    // D2: same id, same handle, in every store.
    assert_eq!(store_a.display_handle(&id_a).unwrap(), "01234567");
    assert_eq!(store_b.display_handle(&id_b).unwrap(), "01234567");
    assert_eq!(store_a.resolve_message_token("01234567").unwrap(), id_a);
    assert_eq!(store_b.resolve_message_token("01234567").unwrap(), id_b);
}

#[test]
fn local_only_ingest_keeps_namespace_seeded_ids() {
    let raw = message_bytes("local@example.com", "body");
    let content_id = sha256_hex(&raw);
    let local = request_in("acct", "inbox", "uid:1", None, None);
    let bound = request_in("acct", "inbox", "uid:1", None, Some(letter_binding(1)));

    assert_eq!(
        ingest::ingest_message_id(&local, &content_id).unwrap(),
        fallback_message_id(&local, &content_id)
    );
    assert_eq!(
        ingest::ingest_message_id(&bound, &content_id).unwrap(),
        remote_bound_message_id("acct", "inbox", &content_id, &letter_binding(1))
    );
}

#[test]
fn malformed_presented_id_is_rejected_before_any_side_effect() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let raw = message_bytes("bad@example.com", "body");
    for bad in [
        "",
        "msg_short",
        "MSG_0123456789abcdef01234567",
        "draft-handle",
    ] {
        let error = storage
            .ingest_message(&request_in("acct", "inbox", "s", Some(bad), None), &raw)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not a traveling id"), "{bad:?}: {error}");
    }
    assert_eq!(storage.blob_count().unwrap(), 0);
    assert_eq!(storage.message_count().unwrap(), 0);
    // The rejected blob was never written to disk either.
    let blob = tmp.path().join(blob_relpath(&sha256_hex(&raw)));
    assert!(!blob.exists());
}

#[test]
fn presented_id_is_idempotent_but_never_swaps_existing_content() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let original = message_bytes("one@example.com", "original");
    let request = request_in("acct", "needs", "s", Some(TRAVELING_ID), None);

    storage.ingest_message(&request, &original).unwrap();
    storage.ingest_message(&request, &original).unwrap();
    assert_eq!(storage.message_count().unwrap(), 1);

    let other = message_bytes("two@example.com", "foreign");
    let error = storage
        .ingest_message(&request, &other)
        .unwrap_err()
        .to_string();
    assert!(error.contains("already names different content"), "{error}");
    let held = storage.list_messages().unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].content_id, sha256_hex(&original));
}

#[test]
fn delivery_batch_refuses_one_presented_id_for_two_recipients() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let raw = message_bytes("batch@example.com", "body");
    let deliver = |storage: &mut Storage, requests: &[MessageIngestRequest]| {
        storage.deliver_raw_batch(requests, &raw, "send", "delivered", "inbox", "subject")
    };

    let clash = [
        request_in("acct-a", "inbox", "s", Some(TRAVELING_ID), None),
        request_in("acct-b", "inbox", "s", Some(TRAVELING_ID), None),
    ];
    let error = deliver(&mut storage, &clash).unwrap_err().to_string();
    assert!(error.contains("more than once"), "{error}");
    assert_eq!(storage.message_count().unwrap(), 0);

    let mixed = [
        request_in("acct-a", "inbox", "s", Some(TRAVELING_ID), None),
        request_in("acct-b", "inbox", "s", None, None),
    ];
    let stored = deliver(&mut storage, &mixed).unwrap();
    assert_eq!(stored[0].message_id, TRAVELING_ID);
    assert_ne!(stored[1].message_id, TRAVELING_ID);
    assert_eq!(storage.message_count().unwrap(), 2);
}

#[test]
fn ingest_if_absent_inserts_once_and_leaves_existing_rows_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let raw = message_bytes("sync@example.com", "body");
    let request = request_in(
        "acct",
        "needs",
        "s",
        Some(TRAVELING_ID),
        Some(letter_binding(3)),
    );
    let event = IngestEvent {
        command: "sync pull",
        event_type: "delivered",
        subject: "subject",
        note: Some("project x"),
    };

    let (first, stored) = storage
        .ingest_if_absent(&request, &raw, Some(&event))
        .unwrap();
    assert_eq!(first, IngestOutcome::Inserted);
    assert_eq!(stored.message_id, TRAVELING_ID);
    storage
        .move_message_to_role("acct", TRAVELING_ID, "done")
        .unwrap();

    let (second, _) = storage
        .ingest_if_absent(&request, &raw, Some(&event))
        .unwrap();
    assert_eq!(second, IngestOutcome::Existing);
    let held = storage.message_by_id(TRAVELING_ID).unwrap().unwrap();
    assert_eq!(held.local_role, "done", "a replay must not reopen the item");
    assert_eq!(storage.remote_binding_count().unwrap(), 1);
    let events = storage.list_mailspace_events(TRAVELING_ID).unwrap();
    assert_eq!(events.len(), 1, "only the insert logs a delivery");
    assert_eq!(events[0].to_role.as_deref(), Some("needs"));
    assert_eq!(events[0].note.as_deref(), Some("project x"));
}

#[test]
fn ingest_if_absent_reports_a_conflict_for_different_content_under_a_held_id() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let request = request_in("acct", "needs", "s", Some(TRAVELING_ID), None);
    let original = message_bytes("one@example.com", "original");
    let foreign = message_bytes("two@example.com", "foreign");
    storage.ingest_if_absent(&request, &original, None).unwrap();

    let (outcome, refused) = storage.ingest_if_absent(&request, &foreign, None).unwrap();

    assert_eq!(outcome, IngestOutcome::Conflict);
    assert!(!outcome.wrote());
    assert_eq!(
        refused.content_id,
        sha256_hex(&foreign),
        "names the newcomer"
    );
    let held = storage.list_messages().unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].content_id, sha256_hex(&original));
    assert_eq!(
        storage.blob_count().unwrap(),
        1,
        "the foreign blob is not kept"
    );
    assert!(!tmp.path().join(&refused.blob_relpath).exists());
}

#[test]
fn evicted_items_restore_in_place_with_their_blob_and_local_state() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let raw = message_bytes("evict@example.com", "body");
    let request = request_in(
        "acct",
        "inbox",
        "s",
        Some(TRAVELING_ID),
        Some(letter_binding(4)),
    );
    let (_, stored) = storage.ingest_if_absent(&request, &raw, None).unwrap();
    storage
        .set_local_read_state("acct", TRAVELING_ID, true)
        .unwrap();
    let blob = tmp.path().join(&stored.blob_relpath);
    assert!(blob.exists());

    let evicted = storage.evict_messages(&[TRAVELING_ID.to_string()]).unwrap();

    assert_eq!(evicted, 1);
    assert!(!blob.exists(), "an unreferenced blob is dropped");
    assert!(storage.list_messages().unwrap().is_empty());
    assert!(storage.resolve_message_token(TRAVELING_ID).is_err());

    let (outcome, restored) = storage.ingest_if_absent(&request, &raw, None).unwrap();

    assert_eq!(outcome, IngestOutcome::Restored);
    assert_eq!(restored.content_id, stored.content_id);
    assert_eq!(std::fs::read(&blob).unwrap(), raw, "byte-identical blob");
    let held = storage.message_by_id(TRAVELING_ID).unwrap().unwrap();
    assert!(held.read_state, "local state survives eviction");
    assert_eq!(storage.remote_binding_count().unwrap(), 1);
}

#[test]
fn a_restore_logs_no_second_delivery_event() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let raw = message_bytes("evict@example.com", "body");
    let request = request_in("acct", "needs", "s", Some(TRAVELING_ID), None);
    let event = IngestEvent {
        command: "sync pull",
        event_type: "delivered",
        subject: "subject",
        note: None,
    };
    storage
        .ingest_if_absent(&request, &raw, Some(&event))
        .unwrap();
    storage.evict_messages(&[TRAVELING_ID.to_string()]).unwrap();

    let (outcome, _) = storage
        .ingest_if_absent(&request, &raw, Some(&event))
        .unwrap();

    assert_eq!(outcome, IngestOutcome::Restored);
    let events = storage.list_mailspace_events(TRAVELING_ID).unwrap();
    assert_eq!(
        events.len(),
        1,
        "the first ingest's event survives eviction"
    );
}

#[test]
fn a_shared_blob_survives_until_its_last_live_row_is_evicted() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let raw = message_bytes("shared@example.com", "body");
    let other_id = "msg_aaaaaaaaaaaaaaaaaaaaaaaa";
    let (_, stored) = storage
        .ingest_if_absent(
            &request_in("a", "inbox", "s", Some(TRAVELING_ID), None),
            &raw,
            None,
        )
        .unwrap();
    storage
        .ingest_if_absent(
            &request_in("b", "inbox", "s", Some(other_id), None),
            &raw,
            None,
        )
        .unwrap();
    let blob = tmp.path().join(&stored.blob_relpath);

    storage.evict_messages(&[TRAVELING_ID.to_string()]).unwrap();
    assert!(blob.exists(), "another live row still needs the blob");

    storage.evict_messages(&[other_id.to_string()]).unwrap();
    assert!(!blob.exists());
}

fn project_binding(account: &str, uid: u32) -> RemoteBindingInput {
    RemoteBindingInput {
        account: account.into(),
        provider: "vivi-project".into(),
        remote_mailbox: "prj-x".into(),
        remote_uid: uid,
        remote_uidvalidity: 1,
    }
}

#[test]
fn bind_remote_binds_an_existing_row_and_leaves_its_state_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let raw = message_bytes("bind@example.com", "body");
    let request = request_in("acct", "needs", "s", Some(TRAVELING_ID), None);
    storage.ingest_if_absent(&request, &raw, None).unwrap();
    storage
        .set_local_read_state("acct", TRAVELING_ID, true)
        .unwrap();
    assert!(
        storage
            .message_by_id(TRAVELING_ID)
            .unwrap()
            .unwrap()
            .remote
            .is_none()
    );

    assert!(
        storage
            .bind_remote(TRAVELING_ID, &project_binding("acct", 7))
            .unwrap()
    );

    let held = storage.message_by_id(TRAVELING_ID).unwrap().unwrap();
    assert_eq!(held.remote, Some(project_binding("acct", 7)));
    assert_eq!(held.local_role, "needs");
    assert!(held.read_state);
    assert_eq!(storage.remote_binding_count().unwrap(), 1);

    // A second bind replaces the first: one binding per message.
    assert!(
        storage
            .bind_remote(TRAVELING_ID, &project_binding("acct", 9))
            .unwrap()
    );
    let held = storage.message_by_id(TRAVELING_ID).unwrap().unwrap();
    assert_eq!(held.remote.unwrap().remote_uid, 9);
    assert_eq!(storage.remote_binding_count().unwrap(), 1);
}

#[test]
fn bind_remote_refuses_a_missing_or_evicted_row() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let raw = message_bytes("gone@example.com", "body");
    assert!(
        !storage
            .bind_remote(TRAVELING_ID, &project_binding("acct", 1))
            .unwrap(),
        "no such row"
    );

    let request = request_in("acct", "inbox", "s", Some(TRAVELING_ID), None);
    storage.ingest_if_absent(&request, &raw, None).unwrap();
    storage.evict_messages(&[TRAVELING_ID.to_string()]).unwrap();
    assert!(
        !storage
            .bind_remote(TRAVELING_ID, &project_binding("acct", 1))
            .unwrap(),
        "an evicted row is not live"
    );
    assert_eq!(storage.remote_binding_count().unwrap(), 0);
}

#[test]
fn bind_remote_reports_a_position_another_row_holds() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let other = "msg_aaaaaaaaaaaaaaaaaaaaaaaa";
    for (id, body) in [(TRAVELING_ID, "one"), (other, "two")] {
        let raw = message_bytes(&format!("{body}@example.com"), body);
        let request = request_in("acct", "inbox", "s", Some(id), None);
        storage.ingest_if_absent(&request, &raw, None).unwrap();
    }
    storage
        .bind_remote(TRAVELING_ID, &project_binding("acct", 5))
        .unwrap();

    assert!(
        storage
            .bind_remote(other, &project_binding("acct", 5))
            .is_err()
    );
    assert!(
        storage
            .message_by_id(other)
            .unwrap()
            .unwrap()
            .remote
            .is_none()
    );
}

#[test]
fn live_lookups_find_rows_by_content_and_by_rfc_message_id() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let raw = message_bytes("Lookup@Example.com", "body");
    let other = "msg_aaaaaaaaaaaaaaaaaaaaaaaa";
    for (account, id) in [("a", TRAVELING_ID), ("b", other)] {
        let request = request_in(account, "inbox", "s", Some(id), None);
        storage.ingest_if_absent(&request, &raw, None).unwrap();
    }
    let content_id = sha256_hex(&raw);

    assert_eq!(
        storage
            .live_messages_for_content(&content_id)
            .unwrap()
            .len(),
        2
    );
    let parent = storage
        .live_message_by_rfc_id("lookup@example.com")
        .unwrap()
        .expect("normalized id matches");
    assert_eq!(parent.content_id, content_id);
    assert!(
        storage
            .live_message_by_rfc_id("missing@example.com")
            .unwrap()
            .is_none()
    );

    storage
        .evict_messages(&[TRAVELING_ID.to_string(), other.to_string()])
        .unwrap();
    assert!(
        storage
            .live_messages_for_content(&content_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        storage
            .live_message_by_rfc_id("lookup@example.com")
            .unwrap()
            .is_none()
    );
}

fn bind_input(uid: u32) -> RemoteBindingInput {
    RemoteBindingInput {
        account: "acct".into(),
        provider: "vivi-project".into(),
        remote_mailbox: "proj".into(),
        remote_uid: uid,
        remote_uidvalidity: 1,
    }
}

#[test]
fn a_batch_that_rolls_back_leaves_no_rows_and_one_that_commits_keeps_them() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();

    storage.begin_batch().unwrap();
    let first = ingest_hinted(&mut storage, "batch-one", 1);
    let second = ingest_hinted(&mut storage, "batch-two", 2);
    assert!(storage.message_by_id(&first).unwrap().is_some());
    storage.rollback_batch().unwrap();

    assert!(storage.message_by_id(&first).unwrap().is_none());
    assert!(storage.message_by_id(&second).unwrap().is_none());
    assert!(
        storage
            .remote_binding_holder(&bind_input(1))
            .unwrap()
            .is_none(),
        "the binding rolled back with the row"
    );

    storage.begin_batch().unwrap();
    let kept = ingest_hinted(&mut storage, "batch-kept", 3);
    storage.commit_batch().unwrap();
    let reopened = Storage::open_mailspace(tmp.path()).unwrap();
    assert!(reopened.message_by_id(&kept).unwrap().is_some());
}

#[test]
fn the_holder_of_a_remote_position_is_found_by_its_binding_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    let held = ingest_hinted(&mut storage, "held-row", 7);

    let mut remote = bind_input(7);
    remote.account = "acct".into();
    remote.provider = "protonmail".into();
    remote.remote_mailbox = "INBOX".into();
    remote.remote_uidvalidity = 42;
    assert_eq!(storage.remote_binding_holder(&remote).unwrap(), Some(held));
    remote.remote_uid = 8;
    assert_eq!(storage.remote_binding_holder(&remote).unwrap(), None);
}

#[test]
fn two_connections_binding_at_once_wait_for_each_other_instead_of_failing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    {
        let mut storage = Storage::open_mailspace(&dir).unwrap();
        for n in 0..120 {
            ingest_unbound(&mut storage, &format!("row-{n}"));
        }
    }
    let workers: Vec<_> = (0..2)
        .map(|worker| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let mut storage = Storage::open_mailspace(&dir).unwrap();
                let mut failures = Vec::new();
                for n in (worker..120).step_by(2) {
                    let uid = u32::try_from(n).unwrap() + 1;
                    let bound = storage.bind_remote(&format!("row-{n}"), &bind_input(uid));
                    if let Err(error) = bound {
                        failures.push(error.to_string());
                    }
                }
                failures
            })
        })
        .collect();

    let failures: Vec<String> = workers
        .into_iter()
        .flat_map(|worker| worker.join().unwrap())
        .collect();

    assert!(
        failures.is_empty(),
        "{} binds failed, first: {:?}",
        failures.len(),
        failures.first()
    );
}

fn ingest_unbound(storage: &mut Storage, message_id: &str) {
    storage
        .ingest_message(
            &MessageIngestRequest {
                account: "acct".into(),
                local_role: "inbox".into(),
                read_state: false,
                starred: false,
                message_id_hint: Some(message_id.to_string()),
                presented_id: None,
                seed_hint: message_id.to_string(),
                remote: None,
            },
            &message_bytes(&format!("{message_id}@example.com"), message_id),
        )
        .unwrap();
}

#[test]
fn batch_event_load_splits_id_lists_beyond_sqlite_parameter_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    ingest_unbound(&mut storage, "batch_events_a");
    ingest_unbound(&mut storage, "batch_events_b");
    for (id, marker) in [
        ("batch_events_a", "probe_one"),
        ("batch_events_b", "probe_two"),
    ] {
        storage
            .append_mailspace_event(&MailspaceEventInput {
                command: "want".into(),
                event_type: marker.into(),
                actor_identity: None,
                account: "acct".into(),
                message_id: id.to_string(),
                content_id: "cnt_probe".into(),
                from_role: None,
                to_role: None,
                from_identity: None,
                to_identity: None,
                subject: "probe".into(),
                note: None,
            })
            .unwrap();
    }

    let mut ids: Vec<String> = (0..40_000).map(|i| format!("filler_{i}")).collect();
    ids.push("batch_events_a".into());
    ids.push("batch_events_b".into());
    let map = storage.list_mailspace_events_for_messages(&ids).unwrap();

    let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["batch_events_a", "batch_events_b"]);
    let types_a: Vec<&str> = map["batch_events_a"]
        .iter()
        .map(|event| event.event_type.as_str())
        .collect();
    assert!(types_a.contains(&"probe_one"));
}

#[test]
fn batch_link_load_splits_id_lists_beyond_sqlite_parameter_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_mailspace(tmp.path()).unwrap();
    ingest_unbound(&mut storage, "batch_link_child");
    ingest_unbound(&mut storage, "batch_link_parent");
    let child = resulting_content_id(&message_bytes(
        "batch_link_child@example.com",
        "batch_link_child",
    ));
    let parent = resulting_content_id(&message_bytes(
        "batch_link_parent@example.com",
        "batch_link_parent",
    ));
    storage
        .link_mailspace_content(&child, &parent, "source")
        .unwrap();

    let mut ids: Vec<String> = (0..40_000).map(|i| format!("filler_{i}")).collect();
    ids.push(child.clone());
    let map = storage.list_mailspace_links_for_children(&ids).unwrap();

    assert_eq!(map.len(), 1);
    let link = &map[&child];
    assert_eq!(link.parent_content_id, parent);
    assert_eq!(link.source, "source");
}
