//! An in-memory project server and real-mailspace fixtures for the sync
//! tests. The fake implements the letter wire's semantics: client ids,
//! sequence order, byte-exact storage, id/hash idempotency, and visibility.
//! Hooks inject the failures the wire can produce.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::chat::b64;
use crate::error::VivariumError;
use crate::mailspace::{Mailspace, SendRequest};
use crate::storage::{StoredMessageView, is_traveling_id, sha256_hex};
use crate::sync::pull::{self, PullReport};
use crate::sync::push::{self, PushReport};
use crate::sync::transport::{AppendFailure, Appended, Envelope, Page, Record, Transport};
use crate::sync::{MAX_LETTER_BYTES, SyncConfig, SyncSettings};

#[derive(Debug, Clone)]
pub struct Item {
    pub sequence: u64,
    pub message_id: String,
    pub from: String,
    pub to: Option<String>,
    pub kind: Option<String>,
    /// The sender's registered prefix, as the server stamps it.
    pub from_prefix: Option<String>,
    pub bytes: Vec<u8>,
    pub sha: String,
}

#[derive(Default)]
struct Inner {
    items: Vec<Item>,
    appends: usize,
    rate_limit_after: Option<usize>,
    refuse: HashMap<String, u16>,
    tampered: HashMap<String, Vec<u8>>,
    page_limit: Option<usize>,
}

#[derive(Default)]
pub struct FakeServer {
    inner: RefCell<Inner>,
}

impl FakeServer {
    #[must_use]
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    pub fn items(&self) -> Vec<Item> {
        self.inner.borrow().items.clone()
    }

    /// How many appends reached the server, accepted or not.
    pub fn append_calls(&self) -> usize {
        self.inner.borrow().appends
    }

    /// Answer 429 to every append after the first `n`.
    pub fn rate_limit_after(&self, n: usize) {
        self.inner.borrow_mut().rate_limit_after = Some(n);
    }

    pub fn lift_rate_limit(&self) {
        self.inner.borrow_mut().rate_limit_after = None;
    }

    /// Answer `status` (400, 409, 413) to appends of `message_id`.
    pub fn refuse(&self, message_id: &str, status: u16) {
        self.inner
            .borrow_mut()
            .refuse
            .insert(message_id.to_string(), status);
    }

    /// Serve at most `n` records per read.
    pub fn page_limit(&self, n: usize) {
        self.inner.borrow_mut().page_limit = Some(n);
    }

    /// Serve `bytes` for `message_id` while keeping the recorded hash.
    pub fn tamper(&self, message_id: &str, bytes: &[u8]) {
        self.inner
            .borrow_mut()
            .tampered
            .insert(message_id.to_string(), bytes.to_vec());
    }

    /// A connection for registered `member`, whose mailspace id begins
    /// `prefix` (8 lowercase hex characters).
    #[must_use]
    pub fn client(self: &Rc<Self>, member: &str, prefix: &str) -> FakeClient {
        FakeClient {
            server: Rc::clone(self),
            member: member.to_string(),
            prefix: Some(prefix.to_string()),
        }
    }

    /// A connection for a member registered before mailspaces had ids: no
    /// prefix to stamp, and nothing can be addressed to it.
    #[must_use]
    pub fn legacy_client(self: &Rc<Self>, member: &str) -> FakeClient {
        FakeClient {
            server: Rc::clone(self),
            member: member.to_string(),
            prefix: None,
        }
    }
}

pub struct FakeClient {
    server: Rc<FakeServer>,
    member: String,
    /// Mail addressed to `<prefix>.local` is visible to this member, and
    /// every letter it appends is stamped with it.
    prefix: Option<String>,
}

impl Transport for FakeClient {
    fn append(&self, message_id: &str, envelope: &Envelope<'_>) -> Result<Appended, AppendFailure> {
        let mut inner = self.server.inner.borrow_mut();
        inner.appends += 1;
        if inner.rate_limit_after.is_some_and(|n| inner.appends > n) {
            return Err(AppendFailure::Stop("HTTP 429 rate_limited".into()));
        }
        if let Some(status) = inner.refuse.get(message_id) {
            return Err(AppendFailure::Refused(format!("HTTP {status} scripted")));
        }
        let bytes = envelope.raw_eml;
        if bytes.is_empty() || !is_traveling_id(message_id) {
            return Err(AppendFailure::Refused("HTTP 400 invalid_request".into()));
        }
        if bytes.len() as u64 > MAX_LETTER_BYTES {
            return Err(AppendFailure::Refused("HTTP 413 body_too_large".into()));
        }
        let sha = sha256_hex(bytes);
        let duplicate_of = |item: &Item| Appended {
            message_id: item.message_id.clone(),
            sequence: item.sequence,
            duplicate: true,
            content_sha256: Some(item.sha.clone()),
        };
        if let Some(held) = inner
            .items
            .iter()
            .find(|item| item.message_id == message_id)
        {
            if held.sha == sha {
                return Ok(duplicate_of(held));
            }
            return Err(AppendFailure::Refused("HTTP 409 conflict".into()));
        }
        // Same bytes under another id is a replay only for the same sender.
        let held_by_sender = inner
            .items
            .iter()
            .find(|item| item.sha == sha && item.from == self.member);
        if let Some(held) = held_by_sender {
            return Ok(duplicate_of(held));
        }
        let sequence = inner.items.len() as u64 + 1;
        inner.items.push(Item {
            sequence,
            message_id: message_id.to_string(),
            from: self.member.clone(),
            to: Some(envelope.to.to_string()),
            kind: Some(envelope.kind.to_string()),
            from_prefix: self.prefix.clone(),
            bytes: bytes.to_vec(),
            sha: sha.clone(),
        });
        Ok(Appended {
            message_id: message_id.to_string(),
            sequence,
            duplicate: false,
            content_sha256: Some(sha),
        })
    }

    fn fetch(&self, since: u64) -> Result<Page, VivariumError> {
        let inner = self.server.inner.borrow();
        let work =
            |kind: &Option<String>| matches!(kind.as_deref(), Some("need" | "task" | "want"));
        let records: Vec<Record> = inner
            .items
            .iter()
            .filter(|item| item.sequence > since)
            .filter(|item| work(&item.kind) || item.from == self.member || self.addressed(item))
            .take(inner.page_limit.unwrap_or(usize::MAX))
            .map(|item| {
                let served = inner.tampered.get(&item.message_id).unwrap_or(&item.bytes);
                Record {
                    sequence: item.sequence,
                    message_id: item.message_id.clone(),
                    from: item.from.clone(),
                    to: item.to.clone(),
                    kind: item.kind.clone(),
                    content_sha256: item.sha.clone(),
                    from_prefix: item.from_prefix.clone(),
                    raw_eml: b64::encode(served),
                }
            })
            .collect();
        let next_cursor = records.last().map_or(since, |record| record.sequence);
        Ok(Page {
            records,
            next_cursor,
        })
    }
}

impl FakeClient {
    /// Whether any envelope address of `item` is in this member's domain.
    fn addressed(&self, item: &Item) -> bool {
        let Some(prefix) = &self.prefix else {
            return false;
        };
        let domain = format!("{prefix}.local");
        item.to.as_deref().is_some_and(|to| {
            to.split(',')
                .filter_map(|address| address.trim().rsplit_once('@'))
                .any(|(_, found)| found == domain)
        })
    }
}

/// A real temp mailspace with sync settings, standing in for one member's
/// checkout.
pub struct Checkout {
    pub mailspace: Mailspace,
    pub settings: SyncSettings,
    _tmp: tempfile::TempDir,
}

pub const PROJECT: &str = "abcde-fghjk-mnpqr-stvwz-23456";

/// A checkout named `name` on the wire, with local `roles`.
pub fn checkout(name: &str, roles: &[&str]) -> Checkout {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    for role in roles {
        mailspace.add_identity(role).unwrap();
    }
    mailspace.config.sync = Some(SyncConfig {
        server: Some("http://sync.test".into()),
        project: Some(PROJECT.into()),
        name: Some(name.into()),
    });
    let settings = SyncSettings::resolve(&mailspace).unwrap();
    Checkout {
        mailspace,
        settings,
        _tmp: tmp,
    }
}

impl Checkout {
    /// This member's connection to `server`.
    pub fn client(&self, server: &Rc<FakeServer>) -> FakeClient {
        server.client(&self.settings.name, self.mailspace.address_prefix())
    }

    /// The address this mailspace mints for `role`.
    pub fn address(&self, role: &str) -> String {
        self.mailspace.address_for(role)
    }

    pub fn push(&self, client: &FakeClient) -> PushReport {
        push::push(&self.mailspace, &self.settings, client).unwrap()
    }

    pub fn pull(&self, client: &FakeClient) -> PullReport {
        pull::pull(&self.mailspace, &self.settings, client).unwrap()
    }

    /// Send a need the way `vivi need send` does; returns the handle the
    /// command prints first: the delivered copy's, else the sent copy's.
    pub fn send_need(&self, from: &str, to: &str, subject: &str) -> String {
        let result = self
            .mailspace
            .send(SendRequest {
                from: from.into(),
                to: vec![to.into()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: subject.into(),
                body: format!("{subject} body\n"),
                role: "needs".into(),
                kind: Some("need".into()),
                reply_to: None,
                depends_on: Vec::new(),
            })
            .unwrap();
        result
            .delivered
            .first()
            .map_or(result.sent, |delivered| delivered.handle.clone())
    }

    /// Compose a plain-mail reply the way `vivi mail reply` does.
    pub fn reply(&self, parent: &str, from: &str, to: &str, body: &str) -> String {
        let result = self
            .mailspace
            .send(SendRequest {
                from: from.into(),
                to: vec![to.into()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "Re: thread".into(),
                body: body.into(),
                role: "inbox".into(),
                kind: Some("mail".into()),
                reply_to: Some(parent.into()),
                depends_on: Vec::new(),
            })
            .unwrap();
        result
            .delivered
            .first()
            .map_or(result.sent, |delivered| delivered.handle.clone())
    }

    /// The full message id a handle names.
    pub fn id_of(&self, handle: &str) -> String {
        self.mailspace
            .storage()
            .unwrap()
            .resolve_message_token(handle)
            .unwrap()
    }

    /// The stored bytes a handle names.
    pub fn bytes_of(&self, handle: &str) -> Vec<u8> {
        self.mailspace
            .storage()
            .unwrap()
            .read_message(&self.id_of(handle))
            .unwrap()
    }

    pub fn row(&self, message_id: &str) -> Option<StoredMessageView> {
        self.mailspace
            .storage()
            .unwrap()
            .message_by_id(message_id)
            .unwrap()
    }

    pub fn rows(&self) -> Vec<StoredMessageView> {
        let mut rows = self.mailspace.storage().unwrap().list_messages().unwrap();
        rows.sort_by(|a, b| a.message_id.cmp(&b.message_id));
        rows
    }

    /// The `.eml` files under the blob store, sorted.
    pub fn blob_files(&self) -> Vec<String> {
        let mut files = Vec::new();
        collect_files(&self.mailspace.dir.join("blobs"), &mut files);
        files.sort();
        files
    }

    pub fn blob_hash(&self, view: &StoredMessageView) -> String {
        sha256_hex(&std::fs::read(self.mailspace.dir.join(&view.blob_relpath)).unwrap())
    }
}

fn collect_files(dir: &std::path::Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path.to_string_lossy().into_owned());
        }
    }
}
