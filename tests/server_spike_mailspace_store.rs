//! Spike for decided items 14 and 16 (docs/phase-03-project-mailspace-hub.md):
//! can the server hold a project as a real vivi mailspace using only the
//! public `vivi` API, keeping each letter's exact bytes and the transport
//! fields (`sequence`, `appended_at`, `from_prefix`), and answer queries from
//! the mailspace index instead of the `.eml` files?
//!
//! Findings are asserted where they are facts and written as comments where
//! they are gaps.

use std::path::PathBuf;

use serde_json::Value;
use sha2::{Digest, Sha256};
use vivi::storage::{
    IngestEvent, IngestOutcome, MessageIngestRequest, RemoteBindingInput, Storage,
};

const PROJECT: &str = "proj_spike";
const PROVIDER: &str = "vivi-server";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vivi-server-spike-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn traveling(n: u64) -> String {
    format!("msg_{n:08x}{:016x}", 0)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A letter as the client writes it: CRLF headers, `X-Vivi-Kind` for work.
fn letter(from: &str, to: &str, kind: Option<&str>, subject: &str, body: &str) -> Vec<u8> {
    let mut eml = format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nDate: Mon, 12 Oct 2026 10:00:00 +0000\r\n"
    );
    if let Some(kind) = kind {
        eml.push_str(&format!("X-Vivi-Kind: {kind}\r\n"));
    }
    eml.push_str(&format!("\r\n{body}\r\n"));
    eml.into_bytes()
}

fn role_for(kind: Option<&str>) -> &'static str {
    match kind {
        Some("need") => "needs",
        Some("task") => "tasks",
        Some("want") => "wants",
        _ => "inbox",
    }
}

/// One append: letter, transport fields, and the store write in a single
/// transaction. The server's append mutex would wrap this; here the caller
/// is single-threaded.
struct Append<'a> {
    id: &'a str,
    bytes: &'a [u8],
    kind: Option<&'a str>,
    sequence: u32,
    appended_at: &'a str,
    from_prefix: &'a str,
}

fn append(store: &mut Storage, item: &Append<'_>) -> IngestOutcome {
    let request = MessageIngestRequest {
        account: PROJECT.to_string(),
        local_role: role_for(item.kind).to_string(),
        read_state: false,
        starred: false,
        message_id_hint: None,
        presented_id: Some(item.id.to_string()),
        seed_hint: String::new(),
        remote: Some(RemoteBindingInput {
            account: PROJECT.to_string(),
            provider: PROVIDER.to_string(),
            remote_mailbox: "items".to_string(),
            remote_uid: item.sequence,
            remote_uidvalidity: 1,
        }),
    };
    // `ingest_if_absent` opens its own transaction, so it cannot sit inside
    // `begin_batch`. Its optional event is written in that same transaction
    // and only when a row is inserted, so the transport fields go in the
    // event note: one atomic write, no event on a replay or a refusal.
    let note = serde_json::json!({
        "sequence": item.sequence,
        "appended_at": item.appended_at,
        "from_prefix": item.from_prefix,
        "content_sha256": sha256_hex(item.bytes),
    })
    .to_string();
    let event = IngestEvent {
        command: "append",
        event_type: "letter_appended",
        subject: "",
        note: Some(&note),
    };
    let (outcome, _) = store
        .ingest_if_absent(&request, item.bytes, Some(&event))
        .unwrap();
    outcome
}

#[test]
fn project_mailspace_holds_letters_verbatim_and_answers_from_the_index() {
    let root = scratch("verbatim");
    let mut store = Storage::open_mailspace(&root).unwrap();

    let need = letter(
        "mind@aaaaaaaa.local",
        "hand@bbbbbbbb.local",
        Some("need"),
        "fix the sync window",
        "done_when: the pull covers a week",
    );
    let want = letter(
        "mind@aaaaaaaa.local",
        "hand@cccccccc.local",
        Some("want"),
        "a nicer prune report",
        "someday",
    );
    let mail = letter(
        "hand@bbbbbbbb.local",
        "mind@aaaaaaaa.local",
        None,
        "re: fix the sync window",
        "on it",
    );
    let (id1, id2, id3) = (traveling(1), traveling(2), traveling(3));
    let t1 = "2026-10-12T10:00:01+00:00";
    let t2 = "2026-10-12T10:00:02+00:00";
    let t3 = "2026-10-12T10:00:03+00:00";

    for (item, expected) in [
        (
            Append {
                id: &id1,
                bytes: &need,
                kind: Some("need"),
                sequence: 1,
                appended_at: t1,
                from_prefix: "aaaaaaaa",
            },
            IngestOutcome::Inserted,
        ),
        (
            Append {
                id: &id2,
                bytes: &want,
                kind: Some("want"),
                sequence: 2,
                appended_at: t2,
                from_prefix: "aaaaaaaa",
            },
            IngestOutcome::Inserted,
        ),
        (
            Append {
                id: &id3,
                bytes: &mail,
                kind: None,
                sequence: 3,
                appended_at: t3,
                from_prefix: "bbbbbbbb",
            },
            IngestOutcome::Inserted,
        ),
    ] {
        assert_eq!(append(&mut store, &item), expected);
    }

    // 1. Bytes: the stored letter is the exact bytes, under its content hash.
    let rows = store
        .list_messages_by_account_role(PROJECT, "needs")
        .unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.message_id, id1, "filed under the presented id");
    assert_eq!(row.content_id, sha256_hex(&need));
    let on_disk = std::fs::read(root.join(&row.blob_relpath)).unwrap();
    assert_eq!(on_disk, need, "byte for byte");

    // 2. The index carries what a listing needs, with no .eml read.
    assert_eq!(row.from_addr, "mind@aaaaaaaa.local");
    assert_eq!(row.to_addr, "hand@bbbbbbbb.local");
    assert_eq!(row.subject, "fix the sync window");
    assert_eq!(row.local_role, "needs");

    // 3. Sequence rides the remote binding uid.
    let remote = row.remote.as_ref().expect("remote binding");
    assert_eq!(remote.remote_uid, 1);
    assert_eq!(remote.provider, PROVIDER);

    // 4. Handles resolve through the store (8 hex after `msg_`).
    let handle = &id1[4..12];
    assert_eq!(store.resolve_message_token(handle).unwrap(), id1);

    // 5. Replay is a no-op; a different body under the same id is refused.
    let replay = Append {
        id: &id1,
        bytes: &need,
        kind: Some("need"),
        sequence: 1,
        appended_at: t1,
        from_prefix: "aaaaaaaa",
    };
    assert_eq!(append(&mut store, &replay), IngestOutcome::Existing);
    assert_eq!(store.list_messages().unwrap().len(), 3, "no duplicate row");
    let other = letter(
        "mind@aaaaaaaa.local",
        "hand@bbbbbbbb.local",
        Some("need"),
        "changed",
        "x",
    );
    let conflict = Append {
        id: &id1,
        bytes: &other,
        kind: Some("need"),
        sequence: 9,
        appended_at: t3,
        from_prefix: "aaaaaaaa",
    };
    assert_eq!(append(&mut store, &conflict), IngestOutcome::Conflict);
    let mut events = Vec::new();
    for id in [&id1, &id2, &id3] {
        events.extend(store.list_mailspace_events(id).unwrap());
    }
    let appended: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "letter_appended")
        .collect();
    assert_eq!(appended.len(), 3, "replay and conflict wrote no event");

    // 6. appended_at and from_prefix come back from the event, in order.
    let mut stamps: Vec<(String, u64, String)> = appended
        .iter()
        .map(|e| {
            let note: Value = serde_json::from_str(e.note.as_deref().unwrap()).unwrap();
            (
                note["appended_at"].as_str().unwrap().to_string(),
                note["sequence"].as_u64().unwrap(),
                note["from_prefix"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    stamps.sort_by_key(|stamp| stamp.1);
    assert_eq!(stamps[0], (t1.to_string(), 1, "aaaaaaaa".to_string()));
    assert_eq!(stamps[2], (t3.to_string(), 3, "bbbbbbbb".to_string()));

    // 7. Recipient scoping from the index: mail for prefix bbbbbbbb only.
    let all = store.list_messages().unwrap();
    let for_b: Vec<_> = all
        .iter()
        .filter(|m| m.to_addr.contains("@bbbbbbbb.local"))
        .collect();
    assert_eq!(for_b.len(), 1);
    assert_eq!(for_b[0].message_id, id1);

    // 8. Survives a reopen.
    drop(store);
    let store = Storage::open_mailspace(&root).unwrap();
    assert_eq!(store.list_messages().unwrap().len(), 3);
    assert_eq!(store.resolve_message_token(handle).unwrap(), id1);

    // Gaps the spike found (see the phase doc, "Open"):
    // - No public `uid > N LIMIT K` or `to = prefix` query: listing filters
    //   the index rows in Rust. Rows, not bytes, so it never reads an .eml,
    //   but it is O(rows) per call until `vivi` grows a cursor query or the
    //   server runs its own SQL beside the mailspace.
    // - `appended_at` and `from_prefix` live in an event note (JSON text),
    //   not in indexed columns. Fine for a transport record, wrong for a
    //   `since=<time>` filter at scale.

    let _ = std::fs::remove_dir_all(&root);
}
