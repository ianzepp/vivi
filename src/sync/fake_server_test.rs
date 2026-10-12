//! An in-memory project server and real-mailspace fixtures for the sync
//! tests. The fake implements the letter wire's semantics: client ids,
//! sequence order, byte-exact storage, id/hash idempotency, visibility,
//! append-time stamps, `limit`/`after` windows with `has_more`, and lookup by
//! handle. Hooks inject the failures the wire can produce.

use chrono::DateTime;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::b64;
use crate::error::VivariumError;
use crate::mailspace::{Mailspace, SendRequest};
use crate::storage::{StoredMessageView, is_traveling_id, sha256_hex};
use crate::sync::pull::{self, PullReport};
use crate::sync::push::{self, PushReport};
use crate::sync::transport::{
    AppendFailure, Appended, Envelope, ListQuery, Lookup, Page, Record, Transport,
};
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
    /// The server clock at append (RFC 3339), as the server stamps it.
    pub appended_at: Option<String>,
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
    /// The server clock new appends are stamped with.
    clock: Option<String>,
    /// Every list request seen: `(since, limit, after)`.
    lists: Vec<(u64, u32, Option<String>)>,
    /// The number of handles in every lookup request seen.
    lookups: Vec<usize>,
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

    /// Stamp appends from now on with this server time (RFC 3339).
    pub fn set_clock(&self, time: &str) {
        self.inner.borrow_mut().clock = Some(time.to_string());
    }

    /// Forget the append time of an item, as for a row written before the
    /// server stamped letters.
    pub fn clear_appended_at(&self, message_id: &str) {
        let mut inner = self.inner.borrow_mut();
        for item in inner
            .items
            .iter_mut()
            .filter(|i| i.message_id == message_id)
        {
            item.appended_at = None;
        }
    }

    /// Every list request seen: `(since, limit, after)`.
    pub fn lists(&self) -> Vec<(u64, u32, Option<String>)> {
        self.inner.borrow().lists.clone()
    }

    /// The handle count of every lookup request seen.
    pub fn lookups(&self) -> Vec<usize> {
        self.inner.borrow().lookups.clone()
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
        let appended_at = inner
            .clock
            .clone()
            .unwrap_or_else(|| "2026-10-11T00:00:00Z".to_string());
        inner.items.push(Item {
            sequence,
            message_id: message_id.to_string(),
            from: self.member.clone(),
            to: Some(envelope.to.to_string()),
            kind: Some(envelope.kind.to_string()),
            from_prefix: self.prefix.clone(),
            appended_at: Some(appended_at),
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

    fn fetch(&self, query: &ListQuery<'_>) -> Result<Page, VivariumError> {
        let mut inner = self.server.inner.borrow_mut();
        inner
            .lists
            .push((query.since, query.limit, query.after.map(String::from)));
        let after = query
            .after
            .map(|time| DateTime::parse_from_rfc3339(time).unwrap());
        let limit = (query.limit as usize).min(inner.page_limit.unwrap_or(usize::MAX));
        let matching: Vec<&Item> = inner
            .items
            .iter()
            .filter(|item| item.sequence > query.since)
            .filter(|item| self.visible(item))
            .filter(|item| {
                after.is_none_or(|after| {
                    item.appended_at
                        .as_deref()
                        .is_some_and(|at| DateTime::parse_from_rfc3339(at).unwrap() >= after)
                })
            })
            .collect();
        let has_more = matching.len() > limit;
        let records: Vec<Record> = matching
            .into_iter()
            .take(limit)
            .map(|item| self.record_of(&inner, item))
            .collect();
        let next_cursor = records.last().map_or(query.since, |record| record.sequence);
        Ok(Page {
            records,
            next_cursor,
            has_more: Some(has_more),
        })
    }

    fn lookup(&self, handles: &[String]) -> Result<Lookup, VivariumError> {
        let mut inner = self.server.inner.borrow_mut();
        inner.lookups.push(handles.len());
        assert!(handles.len() <= 100, "the server takes at most 100 handles");
        let handle_of = |item: &Item| {
            item.message_id
                .strip_prefix("msg_")
                .and_then(|rest| rest.get(..8))
                .map(str::to_ascii_lowercase)
        };
        let mut records: Vec<Record> = Vec::new();
        let mut missing = Vec::new();
        for handle in handles {
            let wanted = handle.to_ascii_lowercase();
            let found: Vec<&Item> = inner
                .items
                .iter()
                .filter(|item| handle_of(item).as_deref() == Some(wanted.as_str()))
                .filter(|item| self.visible(item))
                .collect();
            if found.is_empty() {
                missing.push(handle.clone());
            }
            for item in found {
                if !records.iter().any(|r| r.sequence == item.sequence) {
                    records.push(self.record_of(&inner, item));
                }
            }
        }
        records.sort_by_key(|record| record.sequence);
        Ok(Lookup { records, missing })
    }
}

impl FakeClient {
    /// The visibility rule: work items to everyone, mail to its sender and
    /// to the member whose prefix is on a recipient address.
    fn visible(&self, item: &Item) -> bool {
        let work = matches!(item.kind.as_deref(), Some("need" | "task" | "want"));
        work || item.from == self.member || self.addressed(item)
    }

    fn record_of(&self, inner: &Inner, item: &Item) -> Record {
        let served = inner.tampered.get(&item.message_id).unwrap_or(&item.bytes);
        Record {
            sequence: item.sequence,
            message_id: item.message_id.clone(),
            from: item.from.clone(),
            to: item.to.clone(),
            kind: item.kind.clone(),
            content_sha256: item.sha.clone(),
            from_prefix: item.from_prefix.clone(),
            appended_at: item.appended_at.clone(),
            raw_eml: b64::encode(served),
        }
    }

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

/// `msg_` + 8 hex characters (the handle, `n` in hex) + 16 hex characters.
pub fn wire_id(n: u64, tail: u64) -> String {
    format!("msg_{n:08x}{tail:016x}")
}

/// The handle of `wire_id(n, _)`.
pub fn handle(n: u64) -> String {
    format!("{n:08x}")
}

/// The date every injected letter carries unless told otherwise.
pub const LETTER_DATE: &str = "Sun, 11 Oct 2026 12:00:00 +0000";

/// A plain letter with a unique Message-ID taken from its wire id.
pub fn letter(
    id: &str,
    from: &str,
    to: &str,
    subject: &str,
    body: &str,
    date: &str,
    extra_headers: &str,
) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nMessage-ID: <{id}@x.test>\r\n\
         Date: {date}\r\n{extra_headers}\r\n{body}\r\n"
    )
    .into_bytes()
}

/// One sender, one receiver, and a server that letters are put onto
/// directly: no composing, no push. The receiver owns the roles `mind` and
/// `hand`; every letter is a need addressed to its `mind` unless told
/// otherwise.
pub struct World {
    pub server: Rc<FakeServer>,
    pub alice: Checkout,
    pub bob: Checkout,
    pub bob_client: FakeClient,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    #[must_use]
    pub fn new() -> Self {
        let server = FakeServer::new();
        let alice = checkout("alice", &["mind"]);
        let bob = checkout("bob", &["mind", "hand"]);
        let bob_client = bob.client(&server);
        Self {
            server,
            alice,
            bob,
            bob_client,
        }
    }

    /// Stamp what is put next with this server time.
    pub fn at(&self, time: &str) -> &Self {
        self.server.set_clock(time);
        self
    }

    /// A need for bob's `mind`, under `wire_id(n, 0)`; returns the id.
    pub fn put(&self, n: u64, subject: &str, body: &str) -> String {
        self.put_full(
            &wire_id(n, 0),
            "need",
            &self.bob.address("mind"),
            subject,
            body,
            LETTER_DATE,
            "",
        )
    }

    /// [`World::put`] with a `Date` header.
    pub fn put_dated(&self, n: u64, subject: &str, body: &str, date: &str) -> String {
        self.put_full(
            &wire_id(n, 0),
            "need",
            &self.bob.address("mind"),
            subject,
            body,
            date,
            "",
        )
    }

    /// A letter with every part chosen.
    #[allow(clippy::too_many_arguments)]
    pub fn put_full(
        &self,
        id: &str,
        kind: &str,
        to: &str,
        subject: &str,
        body: &str,
        date: &str,
        extra_headers: &str,
    ) -> String {
        let from = self.alice.address("mind");
        let bytes = letter(id, &from, to, subject, body, date, extra_headers);
        let envelope = Envelope {
            raw_eml: &bytes,
            to,
            kind,
        };
        self.alice
            .client(&self.server)
            .append(id, &envelope)
            .expect("the fake server takes the letter");
        id.to_string()
    }

    /// The saved pull state of bob's mailspace.
    pub fn state(&self) -> crate::sync::state::SyncState {
        crate::sync::state::SyncState::load(
            &crate::sync::sync_dir(&self.bob.mailspace.dir),
            PROJECT,
        )
        .unwrap()
    }

    /// Live row ids of bob's mailspace, sorted.
    pub fn live_ids(&self) -> Vec<String> {
        self.bob
            .rows()
            .into_iter()
            .map(|row| row.message_id)
            .collect()
    }
}
