//! Phase 03 project store (docs/phase-03-project-mailspace-hub.md): the
//! project, not the room, becomes the tenant. One persistent vivi mailspace
//! per project at `DATA_DIR/projects/<project_id>/`, provisioned like the
//! room store. `append` files one item under its client-minted traveling id:
//! content is sealed with the server cipher exactly like room sends
//! (`messages::StoredContent`), rendered as a minimal `.eml` mirroring the
//! vivi chat client's pulled-item rendering, then ingested with
//! `presented_id` so the crate refuses an id that already names different
//! bytes. Per-item sequences are transport-only cursors from a counter
//! persisted in the project sidecar; appends are serialized under a mutex.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vivi::mailspace::Mailspace;
use vivi::storage::{MessageIngestRequest, is_traveling_id};

use crate::admissions::{AdmissionError, AdmissionRegistry};
use crate::crypto::{StoreCipher, set_owner_only};
use crate::ids::{mint_room_id, normalize_room_id};
use crate::project_wake::ProjectWake;

/// Fixed ingest account: server-filed items belong to the project itself.
const ACCOUNT: &str = "project";
/// Reserved administrative addressee (phase-03 item 9); a vouch travels as
/// mail addressed to it.
const POSTMASTER: &str = "postmaster";
/// Sidecar beside `project.json` holding the sequence counter and per-id
/// content digests (digests only, no plaintext).
const INDEX_FILE: &str = "items.json";
/// Ref limits mirror the room-send caps in `messages.rs`.
const MAX_REFS: usize = 32;
const MAX_ID_LEN: usize = 256;

#[derive(Debug, PartialEq, Eq)]
pub enum ProjectError {
    /// Id not canonical Crockford-base32.
    InvalidProjectId,
    /// Unknown project id.
    ProjectNotFound,
    /// Malformed append payload.
    BadRequest(String),
    /// A presented id already names different bytes in this project.
    Conflict(String),
    /// Storage or IO failure.
    Storage(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub project_id: String,
}

/// One item presented for append. `id` is the client-minted traveling id;
/// `kind` is `need` | `task` | `want`, or absent for plain mail.
#[derive(Debug, Clone)]
pub struct AppendItem {
    pub id: String,
    pub from: String,
    /// Recipient name; `None` (or empty) files to the project account.
    /// `postmaster` marks governance mail (vouch processing).
    pub to: Option<String>,
    pub subject: String,
    pub body: String,
    pub kind: Option<String>,
    pub in_reply_to: Option<String>,
    pub refs: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct AppendOutcome {
    pub item_id: String,
    /// Transport-only cursor: monotonic per project, stable across restart.
    pub sequence: u64,
    /// True when the id was already stored with the same bytes.
    pub duplicate: bool,
}

/// One mailspace item returned by the project read path.
#[derive(Debug, PartialEq, Eq)]
pub struct ProjectRecord {
    pub sequence: u64,
    pub message_id: String,
    /// Sender identity, from the sealed content.
    pub from: String,
    /// Recipient name; `None` files to the project account.
    pub to: Option<String>,
    pub kind: Option<String>,
    pub in_reply_to: Option<String>,
    pub refs: Vec<String>,
    pub body: String,
    /// Exact content-addressed `.eml` bytes, suitable for replica ingestion.
    pub raw_eml: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReadAfter {
    pub records: Vec<ProjectRecord>,
    /// Cursor for the last record returned, or the supplied cursor if empty.
    pub next_cursor: u64,
}

/// Content sealed at rest, same shape and sealing as room sends
/// (`messages::StoredContent`).
#[derive(Debug, Serialize, Deserialize)]
struct StoredContent {
    subject: String,
    body: String,
    message_id: String,
    #[serde(default)]
    from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    in_reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    refs: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredRow {
    item_id: String,
    sequence: u64,
    /// sha256 over the canonical content plaintext. The sealed `.eml` bytes
    /// are non-deterministic (fresh AES-GCM nonce per seal), so the digest is
    /// taken before sealing to make same-id replay an exact no-op test.
    content_sha256: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ItemIndex {
    records: Vec<StoredRow>,
    next_sequence: u64,
}

/// Per-project storage root under `DATA_DIR/projects`. Creating a project
/// provisions its mailspace; project metadata persists in `project.json`
/// beside it.
#[derive(Debug, Clone)]
pub struct ProjectStore {
    projects_root: PathBuf,
    admissions: AdmissionRegistry,
    wake: Arc<ProjectWake>,
}

fn append_lock() -> MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn index_path(project_dir: &Path) -> PathBuf {
    project_dir.join(INDEX_FILE)
}

fn read_index(project_dir: &Path) -> Result<ItemIndex, ProjectError> {
    let bytes = match std::fs::read(index_path(project_dir)) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(ItemIndex::default()),
    };
    serde_json::from_slice(&bytes)
        .map_err(|e| ProjectError::Storage(format!("bad items.json: {e}")))
}

fn write_index(project_dir: &Path, index: &ItemIndex) -> Result<(), ProjectError> {
    let bytes = serde_json::to_vec(index)
        .map_err(|e| ProjectError::Storage(format!("failed to encode items.json: {e}")))?;
    let path = index_path(project_dir);
    std::fs::write(&path, &bytes)
        .map_err(|e| ProjectError::Storage(format!("failed to write items.json: {e}")))?;
    set_owner_only(&path);
    Ok(())
}

/// Kind-aware placement (D6), mirroring `messages::placement`.
fn placement(kind: Option<&str>) -> (&'static str, Option<&'static str>) {
    match kind {
        Some("need") => ("needs", Some("need")),
        Some("task") => ("tasks", Some("task")),
        Some("want") => ("wants", Some("want")),
        _ => ("inbox", None),
    }
}

fn valid_ref(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_LEN
}

/// Scoped-read visibility (decided item 7 plus the governance carve-out):
/// work items and mail addressed to postmaster are member-visible; every
/// other item only to its sender or recipient.
fn scoped_visible(content: &StoredContent, viewer: &str) -> bool {
    match content.kind.as_deref() {
        Some("need" | "task" | "want") => true,
        _ => {
            content
                .to
                .as_deref()
                .is_some_and(|to| to.eq_ignore_ascii_case(POSTMASTER))
                || content.from == viewer
                || content.to.as_deref() == Some(viewer)
        }
    }
}

/// Header-safe `<local@domain>` form of a Message-ID, mirroring the vivi
/// chat client's `id_header`.
fn id_header(id: &str, domain: &str) -> String {
    format!("<{}@{domain}>", header_local(id))
}

/// Flatten one header token to its safe local-part form (mirrors the vivi
/// chat client's flattening: allowed punctuation kept, everything else `-`).
fn header_local(id: &str) -> String {
    id.chars()
        .take(200)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Flatten a header value to one safe line.
fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
        .collect()
}

/// Address local-part for the From header.
fn address_local(label: &str) -> String {
    header_local(&single_line(label))
}

/// Minimal `.eml` for one stored item, mirroring the vivi chat client's
/// pulled-item rendering (`vivi::chat::item::render_eml`), with the sealed
/// content as the body so no plaintext reaches mail.sqlite or the blob store.
fn render_eml(project_id: &str, item: &AppendItem, kind: Option<&str>, sealed: &str) -> String {
    let domain = format!("{project_id}.vivi-project");
    let to = item
        .to
        .as_deref()
        .filter(|to| !to.is_empty())
        .unwrap_or(ACCOUNT);
    let mut headers = vec![
        format!("From: {}@{domain}", address_local(&item.from)),
        format!("To: {}@{domain}", address_local(to)),
        format!("Subject: {}", single_line(&item.subject)),
        format!("Date: {}", Utc::now().to_rfc2822()),
        format!("Message-ID: {}", id_header(&item.id, &domain)),
    ];
    if let Some(parent) = item.in_reply_to.as_deref().filter(|p| valid_ref(p)) {
        headers.push(format!("In-Reply-To: {}", id_header(parent, &domain)));
    }
    let refs: Vec<String> = item
        .refs
        .iter()
        .filter(|r| valid_ref(r))
        .take(MAX_REFS)
        .map(|r| id_header(r, &domain))
        .collect();
    if !refs.is_empty() {
        headers.push(format!("References: {}", refs.join(" ")));
    }
    if let Some(kind) = kind {
        headers.push(format!("X-Vivi-Kind: {kind}"));
    }
    headers.push("MIME-Version: 1.0".into());
    headers.push("Content-Type: text/plain; charset=utf-8".into());
    headers.push("Content-Transfer-Encoding: 8bit".into());
    format!("{}\n\n{sealed}", headers.join("\n"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// A governance vouch body: `{"vouch": {"name": ..., "public_key_pem": ...}}`.
#[derive(Deserialize)]
struct Vouch {
    name: String,
    public_key_pem: String,
}

fn parse_vouch(body: &str) -> Result<Vouch, ProjectError> {
    #[derive(Deserialize)]
    struct VouchBody {
        vouch: Vouch,
    }
    serde_json::from_str::<VouchBody>(body)
        .map(|parsed| parsed.vouch)
        .map_err(|_| {
            ProjectError::BadRequest(
            "vouch body must be a JSON object {\"vouch\":{\"name\":...,\"public_key_pem\":...}}"
                .to_owned(),
        )
        })
}

fn admission_error(err: AdmissionError) -> ProjectError {
    match err {
        AdmissionError::InvalidKey => {
            ProjectError::BadRequest("vouch carries an unparsable public key".to_owned())
        }
        AdmissionError::InvalidName => ProjectError::BadRequest(
            "vouched name must be 1-64 chars of letters, digits, dot, dash, or underscore"
                .to_owned(),
        ),
        AdmissionError::ReservedName => {
            ProjectError::BadRequest("the name postmaster is reserved".to_owned())
        }
        AdmissionError::Conflict(message) => ProjectError::Conflict(message),
        AdmissionError::Storage(message) => ProjectError::Storage(message),
    }
}

impl ProjectStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            projects_root: data_dir.join("projects"),
            admissions: AdmissionRegistry::new(data_dir),
            wake: Arc::new(ProjectWake::default()),
        }
    }

    /// Per-project wake registry; fires on every fresh append.
    pub fn wake(&self) -> &ProjectWake {
        &self.wake
    }

    /// Admission registry for this store's data directory.
    pub fn admissions(&self) -> &AdmissionRegistry {
        &self.admissions
    }

    /// Mint a collision-retried project id and create its mailspace.
    pub fn create_project(&self) -> Result<ProjectMeta, ProjectError> {
        for _ in 0..8 {
            let project_id = mint_room_id()
                .map_err(|_| ProjectError::Storage("entropy source unavailable".into()))?;
            let dir = self.projects_root.join(&project_id);
            if dir.exists() {
                continue;
            }
            Mailspace::init(Some(&dir)).map_err(|e| {
                ProjectError::Storage(format!("failed to create project mailspace: {e}"))
            })?;
            let meta = ProjectMeta { project_id };
            let bytes = serde_json::to_vec(&meta).map_err(|e| {
                ProjectError::Storage(format!("failed to encode project.json: {e}"))
            })?;
            std::fs::write(dir.join("project.json"), &bytes)
                .map_err(|e| ProjectError::Storage(format!("failed to write project.json: {e}")))?;
            // Project files (project.json, mailspace sqlite, sidecars) stay
            // owner-only, like the room tree (VC-5).
            crate::crypto::harden_tree(&dir);
            return Ok(meta);
        }
        Err(ProjectError::Storage(
            "project id mint retries exhausted".into(),
        ))
    }

    /// Create a project and admit its creator as the first member (decided
    /// item 8: the first key has no introducer).
    pub fn create_project_with_creator(
        &self,
        public_key_pem: &str,
        creator_name: &str,
    ) -> Result<ProjectMeta, ProjectError> {
        let meta = self.create_project()?;
        self.admissions
            .admit(&meta.project_id, creator_name, public_key_pem, "creator")
            .map_err(admission_error)?;
        Ok(meta)
    }

    /// Canonical project directory for a raw id.
    pub fn project_dir(&self, raw_id: &str) -> Result<PathBuf, ProjectError> {
        let id = normalize_room_id(raw_id).map_err(|_| ProjectError::InvalidProjectId)?;
        Ok(self.projects_root.join(id))
    }

    /// File one item under its presented traveling id. Appends to one project
    /// are serialized by the append lock. Same id plus same bytes is a
    /// duplicate no-op returning the stored sequence; same id plus different
    /// bytes is refused (and the crate's `ensure_presented_id_unclaimed`
    /// backs the same rule inside the mailspace).
    pub fn append(
        &self,
        cipher: &StoreCipher,
        raw_id: &str,
        item: &AppendItem,
    ) -> Result<AppendOutcome, ProjectError> {
        let _guard = append_lock();
        let dir = self.project_dir(raw_id)?;
        if !dir.join("project.json").is_file() {
            return Err(ProjectError::ProjectNotFound);
        }
        let project_id = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        if !is_traveling_id(&item.id) {
            return Err(ProjectError::BadRequest(
                "item id must be a traveling id (msg_ plus 24 lowercase hex)".to_owned(),
            ));
        }
        let kind = match item
            .kind
            .as_deref()
            .map(|k| k.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("") => None,
            Some(k @ ("need" | "task" | "want")) => Some(k.to_owned()),
            Some(other) => {
                return Err(ProjectError::BadRequest(format!(
                    "unsupported kind '{other}'; expected need, task, want, or mail"
                )));
            }
        };
        if item.in_reply_to.as_ref().is_some_and(|r| !valid_ref(r)) {
            return Err(ProjectError::BadRequest(
                "in_reply_to must be a non-empty id of at most 256 bytes".to_owned(),
            ));
        }
        if item.refs.len() > MAX_REFS || !item.refs.iter().all(|r| valid_ref(r)) {
            return Err(ProjectError::BadRequest(format!(
                "refs must hold at most {MAX_REFS} non-empty ids of at most 256 bytes"
            )));
        }
        // Governance vouch (decided item 8): mail addressed to postmaster
        // carries an admission. Validate and write the admission FIRST, then
        // ingest the item — both idempotent, so a crash between them converges
        // on replay.
        if item
            .to
            .as_deref()
            .is_some_and(|to| to.eq_ignore_ascii_case(POSTMASTER))
        {
            let vouch = parse_vouch(&item.body)?;
            if !self.admissions.is_admitted(&project_id, &item.from) {
                return Err(ProjectError::BadRequest(format!(
                    "vouch from '{}': not an admitted member of this project",
                    item.from
                )));
            }
            self.admissions
                .admit(&project_id, &vouch.name, &vouch.public_key_pem, &item.from)
                .map_err(admission_error)?;
        }
        let content = serde_json::to_vec(&StoredContent {
            subject: item.subject.clone(),
            body: item.body.clone(),
            message_id: item.id.clone(),
            from: item.from.clone(),
            to: item.to.clone(),
            kind: kind.clone(),
            in_reply_to: item.in_reply_to.clone(),
            refs: item.refs.clone(),
        })
        .map_err(|e| ProjectError::Storage(format!("failed to encode item content: {e}")))?;
        let content_sha256 = sha256_hex(&content);
        let mut index = read_index(&dir)?;
        if let Some(row) = index.records.iter().find(|r| r.item_id == item.id) {
            if row.content_sha256 == content_sha256 {
                return Ok(AppendOutcome {
                    item_id: item.id.clone(),
                    sequence: row.sequence,
                    duplicate: true,
                });
            }
            return Err(ProjectError::Conflict(format!(
                "item id {} already names different content in this project",
                item.id
            )));
        }
        let sealed = cipher
            .encrypt(&content)
            .map_err(|e| ProjectError::Storage(format!("failed to seal item content: {e}")))?;
        // `reply_to`/`depends_on` equivalents stay out of the ingest on
        // purpose: the crate resolves those against stored handles and would
        // fail the dangling case; threading rides in the sealed content.
        // The ingest request carries the local role only; the mail kind rides
        // in the sealed content and the `X-Vivi-Kind` header, like room sends.
        let (role, _) = placement(kind.as_deref());
        let eml = render_eml(&project_id, item, kind.as_deref(), &sealed);
        let mut storage = Mailspace::init(Some(&dir))
            .map_err(|e| ProjectError::Storage(format!("failed to open project mailspace: {e}")))?
            .storage()
            .map_err(|e| ProjectError::Storage(format!("failed to open project storage: {e}")))?;
        storage
            .ingest_message(
                &MessageIngestRequest {
                    account: ACCOUNT.to_owned(),
                    local_role: role.to_owned(),
                    read_state: false,
                    starred: false,
                    message_id_hint: None,
                    presented_id: Some(item.id.clone()),
                    seed_hint: format!("vivi-project\0{project_id}\0{}", item.id),
                    remote: None,
                },
                eml.as_bytes(),
            )
            .map_err(|e| ProjectError::Storage(format!("item ingest failed: {e}")))?;
        crate::crypto::harden_tree(&dir);
        let sequence = index.next_sequence + 1;
        index.next_sequence = sequence;
        index.records.push(StoredRow {
            item_id: item.id.clone(),
            sequence,
            content_sha256,
        });
        write_index(&dir, &index)?;
        // Fresh state arrived: wake long-poll waiters on this project.
        // Duplicates change nothing and wake nothing.
        self.wake.notify(&project_id);
        Ok(AppendOutcome {
            item_id: item.id.clone(),
            sequence,
            duplicate: false,
        })
    }

    /// Read project items after a transport cursor. Ordering and bytes come
    /// from the project mailspace; the sequence is the one-based position in
    /// that insertion-ordered stream and does not expose sidecar state.
    /// Unscoped: returns every item; internal escape hatch.
    pub fn read_after(
        &self,
        cipher: &StoreCipher,
        raw_id: &str,
        since: u64,
    ) -> Result<ReadAfter, ProjectError> {
        self.read_records(cipher, raw_id, since, None)
    }

    /// Read project items after a transport cursor, scoped to `viewer`
    /// (decided item 7 plus the governance carve-out): work items
    /// (need/task/want) and mail addressed to postmaster are member-visible;
    /// every other item only to its sender or recipient. Sequences keep
    /// read_after's transport meaning — a filtered-out item still holds its
    /// sequence, and `next_cursor` only reflects returned records.
    pub fn read_after_scoped(
        &self,
        cipher: &StoreCipher,
        raw_id: &str,
        since: u64,
        viewer: &str,
    ) -> Result<ReadAfter, ProjectError> {
        self.read_records(cipher, raw_id, since, Some(viewer))
    }

    fn read_records(
        &self,
        cipher: &StoreCipher,
        raw_id: &str,
        since: u64,
        viewer: Option<&str>,
    ) -> Result<ReadAfter, ProjectError> {
        let dir = self.project_dir(raw_id)?;
        if !dir.join("project.json").is_file() {
            return Err(ProjectError::ProjectNotFound);
        }
        let storage = Mailspace::init(Some(&dir))
            .map_err(|e| ProjectError::Storage(format!("failed to open project mailspace: {e}")))?
            .storage()
            .map_err(|e| ProjectError::Storage(format!("failed to open project storage: {e}")))?;
        // Item order comes from the sequence counter table (the transport
        // cursor, decided item 6): the listing API exposes no insertion
        // order, and date+id sorts can diverge from append order on ties.
        // Content still comes from the mailspace alone.
        let index = read_index(&dir)?;
        let mut rows: Vec<&StoredRow> = index
            .records
            .iter()
            .filter(|row| row.sequence > since)
            .collect();
        rows.sort_by_key(|row| row.sequence);
        let messages = storage
            .list_messages()
            .map_err(|e| ProjectError::Storage(format!("failed to list project items: {e}")))?;
        let by_id: std::collections::HashMap<&str, &str> = messages
            .iter()
            .filter(|message| message.account == ACCOUNT)
            .map(|message| (message.message_id.as_str(), message.blob_relpath.as_str()))
            .collect();
        let mut records = Vec::new();
        let mut next_cursor = since;
        for row in rows {
            let Some(blob_relpath) = by_id.get(row.item_id.as_str()) else {
                // The counter row and the store disagree: never serve a
                // sequence the store cannot back.
                return Err(ProjectError::Storage(format!(
                    "sequence {} names item {} absent from the mailspace",
                    row.sequence, row.item_id
                )));
            };
            let raw_eml = storage
                .read_listed_blob(blob_relpath)
                .map_err(|e| ProjectError::Storage(format!("failed to read project blob: {e}")))?;
            let body_start = raw_eml
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|position| position + 2)
                .ok_or_else(|| {
                    ProjectError::Storage("stored project item has no header/body separator".into())
                })?;
            let sealed_body = std::str::from_utf8(&raw_eml[body_start..]).map_err(|e| {
                ProjectError::Storage(format!("stored project item is not UTF-8: {e}"))
            })?;
            let plaintext = cipher.decrypt(sealed_body).map_err(|e| {
                ProjectError::Storage(format!("failed to unseal project item: {e}"))
            })?;
            let content: StoredContent = serde_json::from_slice(&plaintext).map_err(|e| {
                ProjectError::Storage(format!("failed to decode project item: {e}"))
            })?;
            if let Some(viewer) = viewer {
                if !scoped_visible(&content, viewer) {
                    continue;
                }
            }
            next_cursor = row.sequence;
            records.push(ProjectRecord {
                sequence: row.sequence,
                message_id: row.item_id.clone(),
                from: content.from,
                to: content.to,
                kind: content.kind,
                in_reply_to: content.in_reply_to,
                refs: content.refs,
                body: content.body,
                raw_eml,
            });
        }
        Ok(ReadAfter {
            records,
            next_cursor,
        })
    }
}
