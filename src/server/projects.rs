//! Phase 03 project store (docs/phase-03-project-mailspace-hub.md, "Letter
//! wire"): the project, not the room, is the tenant, and it holds an
//! append-only log of sealed letters under `DATA_DIR/projects/<project_id>/`.
//!
//! A letter is the origin's exact `.eml` bytes. The server never parses,
//! renders, or rewrites one (the single exception is mail addressed to
//! `postmaster`, whose body carries a vouch). Each letter is sealed whole
//! with the server cipher into `letters/<sha256>.sealed`; `items.json` holds
//! the envelope rows (`message_id`, `sequence`, `content_sha256`, `from`,
//! `to`, `kind`). The blob is written before its row, so a crash between the
//! two leaves an orphan blob and no row, and a replay converges. Sequences
//! are transport-only cursors; appends are serialized under a mutex.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vivi::storage::is_traveling_id;

use crate::admissions::{AdmissionError, AdmissionRegistry};
use crate::crypto::{StoreCipher, harden_tree};
use crate::identities::{IdentityRecord, MAILSPACE_PREFIX_LEN};
use crate::ids::{mint_room_id, normalize_room_id};
use crate::project_wake::ProjectWake;
use crate::safety::body_over_cap;

/// Reserved administrative addressee (phase-03 item 9); a vouch travels as
/// mail addressed to it.
const POSTMASTER: &str = "postmaster";
/// Envelope index beside `project.json`: sequence counter plus one row per
/// letter (metadata only, no letter bytes).
const INDEX_FILE: &str = "items.json";
/// Directory of sealed letters, one `<content_sha256>.sealed` file each.
const LETTERS_DIR: &str = "letters";

#[derive(Debug, PartialEq, Eq)]
pub enum ProjectError {
    /// Id not canonical Crockford-base32.
    InvalidProjectId,
    /// Unknown project id.
    ProjectNotFound,
    /// Malformed append payload.
    BadRequest(String),
    /// Letter over the 64 KiB cap.
    TooLarge,
    /// A presented id already names different bytes in this project.
    Conflict(String),
    /// Storage or IO failure.
    Storage(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub project_id: String,
}

/// One validated letter presented for append. `from` is the authenticated
/// identity; everything else is sender input checked by [`AppendItem::new`].
#[derive(Debug, Clone)]
pub struct AppendItem {
    id: String,
    from: String,
    from_prefix: Option<String>,
    to: Option<String>,
    kind: Option<String>,
    raw_eml: Vec<u8>,
}

/// At most this many addresses in one `X-Vivi-To`.
const MAX_RECIPIENTS: usize = 16;
/// Cap on the whole `X-Vivi-To` value, in bytes.
const MAX_TO_LEN: usize = 1024;
/// Cap on the local and the domain part of an address, in bytes.
const MAX_ADDRESS_PART: usize = 64;

/// `local@domain`: local `[A-Za-z0-9._-]{1,64}`, domain `[A-Za-z0-9.-]{1,64}`.
fn valid_address(address: &str) -> bool {
    let Some((local, domain)) = address.split_once('@') else {
        return false;
    };
    (1..=MAX_ADDRESS_PART).contains(&local.len())
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && (1..=MAX_ADDRESS_PART).contains(&domain.len())
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// The addresses of a stored `to` value; entries are trimmed.
fn recipients(to: &str) -> impl Iterator<Item = &str> {
    to.split(',').map(str::trim)
}

/// Validate the envelope recipient header. Absent or blank is no recipient;
/// otherwise `postmaster` (any case) or 1 to 16 comma-separated addresses.
/// The trimmed value is stored verbatim.
fn parse_to(to: Option<&str>) -> Result<Option<String>, ProjectError> {
    let Some(to) = to.map(str::trim).filter(|to| !to.is_empty()) else {
        return Ok(None);
    };
    let ok = to.len() <= MAX_TO_LEN
        && (to.eq_ignore_ascii_case(POSTMASTER)
            || (recipients(to).count() <= MAX_RECIPIENTS && recipients(to).all(valid_address)));
    if ok {
        Ok(Some(to.to_owned()))
    } else {
        Err(ProjectError::BadRequest(format!(
            "to must be postmaster or 1-{MAX_RECIPIENTS} comma-separated local@domain addresses \
             (at most {MAX_TO_LEN} bytes)"
        )))
    }
}

/// The mailspace prefix an address domain names: exactly `<8 hex>.local`
/// (case-insensitive), else `None`.
fn domain_prefix(domain: &str) -> Option<&str> {
    let (head, tld) = domain.split_once('.')?;
    (head.len() == MAILSPACE_PREFIX_LEN
        && head.bytes().all(|b| b.is_ascii_hexdigit())
        && tld.eq_ignore_ascii_case("local"))
    .then_some(head)
}

/// True when any recipient address in `to` names `prefix`'s mailspace.
fn names_prefix(to: &str, prefix: &str) -> bool {
    recipients(to)
        .filter_map(|address| address.split_once('@'))
        .filter_map(|(_, domain)| domain_prefix(domain))
        .any(|named| named.eq_ignore_ascii_case(prefix))
}

/// Who is reading: the authenticated identity name and the mailspace prefix
/// bound to it at registration (none for a legacy identity).
#[derive(Debug, Clone, Copy)]
pub struct Viewer<'a> {
    pub name: &'a str,
    pub prefix: Option<&'a str>,
}

impl<'a> Viewer<'a> {
    pub fn of(identity: &'a IdentityRecord) -> Self {
        Self {
            name: &identity.name,
            prefix: identity.mailspace_prefix(),
        }
    }
}

impl AppendItem {
    /// Validate one letter and its envelope. `id` is the client-minted
    /// traveling id; `kind` is `need` | `task` | `want` (case-insensitive),
    /// or `mail`/empty/absent for plain mail, recorded as no kind; `to` is
    /// the optional envelope recipient: `postmaster` or a comma-separated
    /// list of `local@domain` addresses whose domains need not be registered.
    pub fn new(
        id: &str,
        from: &str,
        to: Option<&str>,
        kind: Option<&str>,
        raw_eml: Vec<u8>,
    ) -> Result<Self, ProjectError> {
        if !is_traveling_id(id) {
            return Err(ProjectError::BadRequest(
                "item id must be a traveling id (msg_ plus 24 lowercase hex)".to_owned(),
            ));
        }
        if raw_eml.is_empty() {
            return Err(ProjectError::BadRequest("letter is empty".to_owned()));
        }
        if body_over_cap(raw_eml.len()) {
            return Err(ProjectError::TooLarge);
        }
        let kind = match kind.map(|k| k.trim().to_ascii_lowercase()).as_deref() {
            None | Some("" | "mail") => None,
            Some(k @ ("need" | "task" | "want")) => Some(k.to_owned()),
            Some(other) => {
                return Err(ProjectError::BadRequest(format!(
                    "unsupported kind '{other}'; expected need, task, want, or mail"
                )));
            }
        };
        let to = parse_to(to)?;
        Ok(Self {
            id: id.to_owned(),
            from: from.to_owned(),
            from_prefix: None,
            to,
            kind,
            raw_eml,
        })
    }

    /// Stamp the sender's registered mailspace prefix (none for a legacy
    /// identity). It is recorded beside the letter at append and never
    /// recomputed, so a receiver can check the letter's From domain against it.
    pub fn with_from_prefix(mut self, prefix: Option<&str>) -> Self {
        self.from_prefix = prefix.map(str::to_owned);
        self
    }

    fn is_governance(&self) -> bool {
        self.to
            .as_deref()
            .is_some_and(|to| to.eq_ignore_ascii_case(POSTMASTER))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct AppendOutcome {
    /// The id the letter is stored under: the presented id, or the original
    /// id when the same bytes were already held under another one.
    pub message_id: String,
    /// Transport-only cursor: monotonic per project, stable across restart.
    pub sequence: u64,
    /// Lowercase hex sha256 of the letter bytes.
    pub content_sha256: String,
    /// True when the letter was already stored (same id and bytes, or the
    /// same bytes under another id).
    pub duplicate: bool,
}

/// One letter returned by the project read path.
#[derive(Debug, PartialEq, Eq)]
pub struct ProjectRecord {
    pub sequence: u64,
    pub message_id: String,
    pub from: String,
    /// The sender's mailspace prefix as of the append; `None` for a legacy
    /// identity.
    pub from_prefix: Option<String>,
    pub to: Option<String>,
    pub kind: Option<String>,
    pub content_sha256: String,
    /// The exact bytes that were appended.
    pub raw_eml: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReadAfter {
    pub records: Vec<ProjectRecord>,
    /// Cursor for the last record returned, or the supplied cursor if empty.
    pub next_cursor: u64,
}

/// One envelope row of the index.
#[derive(Debug, Serialize, Deserialize)]
struct StoredRow {
    message_id: String,
    sequence: u64,
    content_sha256: String,
    from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    from_prefix: Option<String>,
    to: Option<String>,
    kind: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ItemIndex {
    records: Vec<StoredRow>,
    next_sequence: u64,
}

/// Per-project storage root under `DATA_DIR/projects`; project metadata
/// persists in `project.json` beside the letter log.
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

fn read_index(project_dir: &Path) -> Result<ItemIndex, ProjectError> {
    let bytes = match std::fs::read(project_dir.join(INDEX_FILE)) {
        Ok(bytes) => bytes,
        // A project with no appends has no index yet; any other failure must
        // not read as "empty" or the next append would reuse sequences.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ItemIndex::default()),
        Err(e) => {
            return Err(ProjectError::Storage(format!(
                "failed to read items.json: {e}"
            )));
        }
    };
    serde_json::from_slice(&bytes)
        .map_err(|e| ProjectError::Storage(format!("bad items.json: {e}")))
}

fn write_index(project_dir: &Path, index: &ItemIndex) -> Result<(), ProjectError> {
    let bytes = serde_json::to_vec(index)
        .map_err(|e| ProjectError::Storage(format!("failed to encode items.json: {e}")))?;
    write_atomic(&project_dir.join(INDEX_FILE), &bytes)
}

fn letter_path(project_dir: &Path, content_sha256: &str) -> PathBuf {
    project_dir
        .join(LETTERS_DIR)
        .join(format!("{content_sha256}.sealed"))
}

/// Write `bytes` to `path` as one 0600 file: temp file, fsync, rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ProjectError> {
    let tmp = path.with_extension("tmp");
    write_private(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| ProjectError::Storage(format!("failed to write {}: {e}", path.display())))
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

/// Scoped-read visibility (decided items 7 and 11 plus the governance
/// carve-out), decided from the envelope row alone: work items and mail
/// addressed to postmaster are member-visible; every other letter only to
/// its sender (by name) or to the member whose registered prefix is named by
/// a recipient address.
fn scoped_visible(row: &StoredRow, viewer: &Viewer<'_>) -> bool {
    match row.kind.as_deref() {
        Some("need" | "task" | "want") => true,
        _ => {
            row.from == viewer.name
                || row.to.as_deref().is_some_and(|to| {
                    to.eq_ignore_ascii_case(POSTMASTER)
                        || viewer.prefix.is_some_and(|prefix| names_prefix(to, prefix))
                })
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The bytes after the first blank line (`\r\n\r\n` or `\n\n`, whichever
/// occurs first), or `None` when the letter has no blank line.
fn letter_body(raw: &[u8]) -> Option<&[u8]> {
    let crlf = raw.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4);
    let lf = raw.windows(2).position(|w| w == b"\n\n").map(|p| p + 2);
    let start = match (crlf, lf) {
        (Some(a), Some(b)) => a.min(b),
        (a, b) => a.or(b)?,
    };
    Some(&raw[start..])
}

/// A governance vouch body: `{"vouch": {"name": ..., "public_key_pem": ...}}`.
#[derive(Deserialize)]
struct Vouch {
    name: String,
    public_key_pem: String,
}

fn bad_vouch() -> ProjectError {
    ProjectError::BadRequest(
        "vouch body must be a JSON object {\"vouch\":{\"name\":...,\"public_key_pem\":...}}"
            .to_owned(),
    )
}

/// Parse the vouch out of a letter addressed to postmaster: the body after
/// the first blank line, required to be UTF-8 JSON.
fn parse_vouch(raw: &[u8]) -> Result<Vouch, ProjectError> {
    #[derive(Deserialize)]
    struct VouchBody {
        vouch: Vouch,
    }
    let body = letter_body(raw).ok_or_else(bad_vouch)?;
    let body = std::str::from_utf8(body).map_err(|_| bad_vouch())?;
    serde_json::from_str::<VouchBody>(body)
        .map(|parsed| parsed.vouch)
        .map_err(|_| bad_vouch())
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

/// Replay rules against the index: the same id with the same bytes is a
/// duplicate, the same id with other bytes conflicts, and an unseen id with
/// bytes the SAME sender already holds under another id is a duplicate of
/// that original. The last rule is sender-scoped on purpose: answering any
/// member would let one who holds the exact bytes of mail they cannot see
/// confirm it exists and learn its id and sequence.
fn find_replay(
    index: &ItemIndex,
    id: &str,
    from: &str,
    content_sha256: &str,
) -> Result<Option<AppendOutcome>, ProjectError> {
    let duplicate = |row: &StoredRow| AppendOutcome {
        message_id: row.message_id.clone(),
        sequence: row.sequence,
        content_sha256: row.content_sha256.clone(),
        duplicate: true,
    };
    if let Some(row) = index.records.iter().find(|r| r.message_id == id) {
        if row.content_sha256 == content_sha256 {
            return Ok(Some(duplicate(row)));
        }
        return Err(ProjectError::Conflict(format!(
            "item id {id} already names a different letter in this project"
        )));
    }
    Ok(index
        .records
        .iter()
        .find(|r| r.from == from && r.content_sha256 == content_sha256)
        .map(duplicate))
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

    /// Mint a collision-retried project id and create its letter log.
    pub fn create_project(&self) -> Result<ProjectMeta, ProjectError> {
        for _ in 0..8 {
            let project_id = mint_room_id()
                .map_err(|_| ProjectError::Storage("entropy source unavailable".into()))?;
            let dir = self.projects_root.join(&project_id);
            if dir.exists() {
                continue;
            }
            std::fs::create_dir_all(dir.join(LETTERS_DIR)).map_err(|e| {
                ProjectError::Storage(format!("failed to create project directory: {e}"))
            })?;
            let meta = ProjectMeta { project_id };
            let bytes = serde_json::to_vec(&meta).map_err(|e| {
                ProjectError::Storage(format!("failed to encode project.json: {e}"))
            })?;
            std::fs::write(dir.join("project.json"), &bytes)
                .map_err(|e| ProjectError::Storage(format!("failed to write project.json: {e}")))?;
            // Project files (project.json, sidecars, letters) stay
            // owner-only, like the room tree (VC-5).
            harden_tree(&dir);
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

    /// Canonical project directory, required to hold an existing project.
    pub fn existing_project_dir(&self, raw_id: &str) -> Result<PathBuf, ProjectError> {
        let dir = self.project_dir(raw_id)?;
        if dir.join("project.json").is_file() {
            Ok(dir)
        } else {
            Err(ProjectError::ProjectNotFound)
        }
    }

    /// Process the governance vouch in a letter to postmaster: validate it
    /// and write the admission. Idempotent, so a replay after a crash
    /// between this and the index write converges.
    fn process_vouch(&self, project_id: &str, item: &AppendItem) -> Result<(), ProjectError> {
        let vouch = parse_vouch(&item.raw_eml)?;
        if !self.admissions.is_admitted(project_id, &item.from) {
            return Err(ProjectError::BadRequest(format!(
                "vouch from '{}': not an admitted member of this project",
                item.from
            )));
        }
        self.admissions
            .admit(project_id, &vouch.name, &vouch.public_key_pem, &item.from)
            .map_err(admission_error)
    }

    /// Append one letter under its presented traveling id. Appends are
    /// serialized by the append lock. The decision matrix is in
    /// [`find_replay`]; a fresh letter is sealed whole into its blob first,
    /// then recorded as the next index row.
    pub fn append(
        &self,
        cipher: &StoreCipher,
        raw_id: &str,
        item: &AppendItem,
    ) -> Result<AppendOutcome, ProjectError> {
        let _guard = append_lock();
        let dir = self.existing_project_dir(raw_id)?;
        let project_id = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        let content_sha256 = sha256_hex(&item.raw_eml);
        let mut index = read_index(&dir)?;
        if let Some(outcome) = find_replay(&index, &item.id, &item.from, &content_sha256)? {
            return Ok(outcome);
        }
        // Governance vouch (decided item 8): the admission is written FIRST,
        // then the letter is stored; both are idempotent, so a crash between
        // them converges on replay.
        if item.is_governance() {
            self.process_vouch(&project_id, item)?;
        }
        let sealed = cipher
            .encrypt(&item.raw_eml)
            .map_err(|e| ProjectError::Storage(format!("failed to seal letter: {e}")))?;
        std::fs::create_dir_all(dir.join(LETTERS_DIR)).map_err(|e| {
            ProjectError::Storage(format!("failed to prepare letters directory: {e}"))
        })?;
        write_atomic(&letter_path(&dir, &content_sha256), sealed.as_bytes())?;
        let sequence = index.next_sequence + 1;
        index.next_sequence = sequence;
        index.records.push(StoredRow {
            message_id: item.id.clone(),
            sequence,
            content_sha256: content_sha256.clone(),
            from: item.from.clone(),
            from_prefix: item.from_prefix.clone(),
            to: item.to.clone(),
            kind: item.kind.clone(),
        });
        write_index(&dir, &index)?;
        // Fresh state arrived: wake long-poll waiters on this project.
        // Duplicates change nothing and wake nothing.
        self.wake.notify(&project_id);
        Ok(AppendOutcome {
            message_id: item.id.clone(),
            sequence,
            content_sha256,
            duplicate: false,
        })
    }

    /// Read letters after a transport cursor, scoped to `viewer` (decided
    /// items 7 and 11 plus the governance carve-out): work items
    /// (need/task/want) and mail addressed to postmaster are member-visible;
    /// every other letter only to its sender or the recipient prefix's
    /// member. Visibility is decided from
    /// the index row, so a letter the viewer cannot see is never unsealed. A
    /// filtered-out letter still holds its sequence, and `next_cursor` only
    /// reflects returned records.
    pub fn read_after_scoped(
        &self,
        cipher: &StoreCipher,
        raw_id: &str,
        since: u64,
        viewer: &Viewer<'_>,
    ) -> Result<ReadAfter, ProjectError> {
        let dir = self.existing_project_dir(raw_id)?;
        let index = read_index(&dir)?;
        let mut rows: Vec<&StoredRow> = index
            .records
            .iter()
            .filter(|row| row.sequence > since && scoped_visible(row, viewer))
            .collect();
        rows.sort_by_key(|row| row.sequence);
        let mut records = Vec::with_capacity(rows.len());
        let mut next_cursor = since;
        for row in rows {
            let raw_eml = read_letter(cipher, &dir, row)?;
            next_cursor = row.sequence;
            records.push(record_of(row, raw_eml));
        }
        Ok(ReadAfter {
            records,
            next_cursor,
        })
    }

    /// Read one letter by id, scoped to `viewer` with the same rules as
    /// [`read_after_scoped`](Self::read_after_scoped). A letter the viewer
    /// may not see is `None`, exactly like an unknown id, and is decided
    /// from the index row so it is never unsealed.
    pub fn read_one_scoped(
        &self,
        cipher: &StoreCipher,
        raw_id: &str,
        message_id: &str,
        viewer: &Viewer<'_>,
    ) -> Result<Option<ProjectRecord>, ProjectError> {
        let dir = self.existing_project_dir(raw_id)?;
        let index = read_index(&dir)?;
        let Some(row) = index
            .records
            .iter()
            .find(|row| row.message_id == message_id && scoped_visible(row, viewer))
        else {
            return Ok(None);
        };
        let raw_eml = read_letter(cipher, &dir, row)?;
        Ok(Some(record_of(row, raw_eml)))
    }
}

fn record_of(row: &StoredRow, raw_eml: Vec<u8>) -> ProjectRecord {
    ProjectRecord {
        sequence: row.sequence,
        message_id: row.message_id.clone(),
        from: row.from.clone(),
        from_prefix: row.from_prefix.clone(),
        to: row.to.clone(),
        kind: row.kind.clone(),
        content_sha256: row.content_sha256.clone(),
        raw_eml,
    }
}

/// Unseal the blob an index row names. A missing, unreadable, or mismatched
/// blob is a storage failure: never serve a sequence the store cannot back.
fn read_letter(
    cipher: &StoreCipher,
    project_dir: &Path,
    row: &StoredRow,
) -> Result<Vec<u8>, ProjectError> {
    let sealed =
        std::fs::read_to_string(letter_path(project_dir, &row.content_sha256)).map_err(|e| {
            ProjectError::Storage(format!(
                "sequence {} names letter {} that cannot be read: {e}",
                row.sequence, row.content_sha256
            ))
        })?;
    let raw_eml = cipher
        .decrypt(&sealed)
        .map_err(|e| ProjectError::Storage(format!("failed to unseal letter: {e}")))?;
    if sha256_hex(&raw_eml) != row.content_sha256 {
        return Err(ProjectError::Storage(format!(
            "letter blob for sequence {} does not match its content hash",
            row.sequence
        )));
    }
    Ok(raw_eml)
}
