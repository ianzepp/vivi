//! Tests for the hosted-index pipeline foundation: the scratch host
//! fixture, the send and reconcile helpers, and the per-concern test
//! modules.

use std::path::Path;

use serde_json::json;
use tempfile::TempDir;

use super::model::{
    CandidateRow, ContentGroup, Discovery, Frontier, PreparedLetter, Selection, SendOutcome,
    WaitingOn,
};
use super::rfc;
use super::{IndexOutcome, IndexSkip};
use crate::mailspace::Mailspace;
use crate::server::projects::ProjectStore;
use crate::storage::{ChangeWatermark, MessageIngestRequest, StoredMessage};
use crate::sync::select::Letter;

/// A scratch host: a fresh host mailspace and a registered hosted project
/// re-pointed at it, ready to index.
struct Host {
    /// Holds the server data dir alive until drop.
    #[allow(dead_code)]
    data: TempDir,
    root: TempDir,
    store: ProjectStore,
    id: String,
    mailspace: Mailspace,
}

impl Host {
    fn new(label: &str) -> Self {
        let data = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut mailspace = Mailspace::init(Some(root.path())).unwrap();
        mailspace.add_identity("member").unwrap();
        let store = ProjectStore::new(data.path());
        let id = store.create_project().unwrap().project_id;
        store.register_hosted(&id, "member", None);
        repoint(store.project_dir(&id).unwrap().join("project.json"), root.path());
        Self {
            data,
            root,
            store,
            id,
            mailspace,
        }
    }

    /// Send one plain mail letter into the host mailspace: a fresh,
    /// unbound row the next pass can index.
    fn send(&self, subject: &str) -> StoredMessage {
        let eml = letter(subject);
        self.mailspace
            .storage()
            .unwrap()
            .ingest_message(
                &MessageIngestRequest {
                    account: "member".into(),
                    local_role: "inbox".into(),
                    read_state: false,
                    starred: false,
                    message_id_hint: None,
                    presented_id: None,
                    seed_hint: format!("hosted-index-test\0{subject}"),
                    remote: None,
                },
                &eml,
            )
            .unwrap()
    }

    /// Reconcile the index with the host: run one pass and demand success.
    fn reconcile(&self) -> IndexOutcome {
        self.store.index_hosted(&self.id).unwrap()
    }

    /// The host mailspace root and identity, as a completed frontier reads.
    fn frontier_now(&self) -> Frontier {
        Frontier {
            root: self.root.path().to_path_buf(),
            mailspace_id: self.mailspace.mailspace_id().to_owned(),
            upper: self.watermark(),
        }
    }

    fn watermark(&self) -> ChangeWatermark {
        self.mailspace
            .storage()
            .unwrap()
            .change_watermark()
            .unwrap()
    }
}

/// Point a project's `project.json` at the host root, the way the server's
/// hosted join writes it.
fn repoint(project_json: &Path, root: &Path) {
    let mut meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(project_json).unwrap()).unwrap();
    meta["mailspace_root"] = json!(root.display().to_string());
    std::fs::write(project_json, serde_json::to_vec(&meta).unwrap()).unwrap();
}

/// The bytes of a small plain mail letter, addressed off the host.
fn letter(subject: &str) -> Vec<u8> {
    format!(
        "From: member@x.test\r\nTo: hand@x.test\r\nSubject: {subject}\r\n\
         Message-ID: <{subject}@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\n\
         body of {subject}\r\n"
    )
    .into_bytes()
}

/// The foundation holds together end to end: the scratch host reconciles a
/// sent letter into the index and then goes quiet, and every model type
/// carries its load across the pass's seams.
#[test]
fn scratch_host_reconciles_and_model_types_carry_their_load() {
    let host = Host::new("smoke");
    let stored = host.send("smoke-letter");
    let first = host.reconcile();
    assert_eq!(
        first,
        IndexOutcome::Ran {
            indexed: 1,
            duplicates: 0,
            refused: 0,
            stopped: None,
        }
    );
    // The pass's own binding may move the watermark, so let one pass settle
    // the books; after that the host is quiet.
    let _ = host.reconcile();
    assert_eq!(host.reconcile(), IndexOutcome::Skipped(IndexSkip::Unchanged));

    // A candidate row narrows to identity and binding, and carries the
    // normalized RFC id dependent events wake on.
    let row = CandidateRow {
        handle: "h_smoke".into(),
        content_id: stored.content_id.clone(),
        account: "member".into(),
        rfc_id: rfc::normalize("<SMOKE-LETTER@x.test>"),
        remote: None,
    };
    assert_eq!(row.rfc_id.as_deref(), Some("smoke-letter@x.test"));

    // Discovery groups the rows by content and samples an upper mark; the
    // frontier records the root, the mailspace identity, and that mark.
    let group = ContentGroup {
        content_id: stored.content_id.clone(),
        rows: vec![row.clone()],
    };
    let discovery = Discovery {
        upper: host.watermark(),
        groups: vec![group.clone()],
    };
    let mut frontier = host.frontier_now();
    frontier.upper = discovery.upper;
    assert_eq!(discovery.groups, vec![group]);
    assert_eq!(frontier, host.frontier_now());

    // Selection keys the ready letters and the waiting dependencies by
    // content id; the send reports what was bound.
    let prepared = PreparedLetter {
        letter: Letter {
            wire_id: stored.message_id.clone(),
            handle: "h_smoke".into(),
            content_id: stored.content_id.clone(),
            account: "member".into(),
            kind: "mail".into(),
            to: "hand@x.test".into(),
        },
        blob_path: host
            .root
            .path()
            .join(".vivi")
            .join(&stored.blob_relpath),
    };
    let mut selection = Selection::default();
    selection
        .ready
        .insert(stored.content_id.clone(), prepared.clone());
    selection.waiting.insert(
        "cid-wake".into(),
        WaitingOn {
            wakes_on: rfc::normalize("<WAKE@x.test>").unwrap(),
        },
    );
    let mut sent = SendOutcome::default();
    sent.bound.insert(stored.content_id.clone());
    assert_eq!(selection.ready.get(&stored.content_id), Some(&prepared));
    assert_eq!(
        selection.waiting["cid-wake"].wakes_on,
        "wake@x.test".to_owned()
    );
    assert_eq!(sent.bound, [stored.content_id.clone()].into_iter().collect());
}

#[path = "work_test.rs"]
mod work;
#[path = "frontier_test.rs"]
mod frontier;
#[path = "failure_test.rs"]
mod failure;
