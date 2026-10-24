//! Phase 03 project store (docs/phase-03-project-mailspace-hub.md, decided
//! items 14 and 16): the project, not the room, is the tenant, and it is a
//! real vivi mailspace under `DATA_DIR/projects/<project_id>/.vivi/`.
//!
//! A letter is the origin's exact `.eml` bytes. The server never rewrites
//! one (the single exception is mail addressed to `postmaster`, whose body
//! carries a vouch). Each letter is ingested verbatim into the project's
//! mailspace under its presented id, so the mailspace index answers from, to,
//! subject and handle without reading an `.eml`. The transport record the
//! wire needs (`sequence`, `appended_at`, the sender's name and prefix, the
//! envelope recipients) lives in `transport.sqlite` beside the mailspace,
//! with indexed columns, so a list is one indexed query and never a scan.
//! The mailspace write comes first, then the transport row; both are
//! idempotent by id, so a crash between them converges on replay. Sequences
//! are transport-only cursors; appends are serialized under a mutex.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Timelike, Utc};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vivi::letter::{LetterError, of_message};
use vivi::mailspace::Mailspace;
use vivi::storage::{IngestOutcome, MessageIngestRequest, Storage, is_traveling_id};

use crate::admissions::{AdmissionError, AdmissionRegistry};
use crate::crypto::harden_tree;
use crate::identities::{IdentityRecord, MAILSPACE_PREFIX_LEN};
use crate::ids::{mint_room_id, normalize_room_id};
use crate::project_wake::ProjectWake;
use crate::safety::body_over_cap;

mod hosted_index;
pub use hosted_index::{IndexOutcome, IndexSkip};

/// Reserved administrative addressee (phase-03 item 9); a vouch travels as
/// mail addressed to it.
const POSTMASTER: &str = "postmaster";
/// Transport index beside `project.json`: one row per letter with the
/// sequence, sender, envelope recipients, kind, and append stamp.
const TRANSPORT_FILE: &str = "transport.sqlite";
/// The mailspace directory inside a project directory, as for a room.
const MAILSPACE_DIR: &str = ".vivi";

const TRANSPORT_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS items (
      sequence INTEGER PRIMARY KEY,
      message_id TEXT NOT NULL UNIQUE,
      handle TEXT NOT NULL,
      content_sha256 TEXT NOT NULL,
      byte_size INTEGER NOT NULL,
      from_name TEXT NOT NULL,
      from_prefix TEXT,
      to_raw TEXT,
      kind TEXT,
      appended_at TEXT NOT NULL,
      event INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS items_handle ON items(handle);
    CREATE INDEX IF NOT EXISTS items_sender_content ON items(from_name, content_sha256);
    CREATE INDEX IF NOT EXISTS items_appended ON items(appended_at);
    CREATE TABLE IF NOT EXISTS recipients (
      sequence INTEGER NOT NULL REFERENCES items(sequence),
      address TEXT NOT NULL,
      prefix TEXT,
      PRIMARY KEY (sequence, address)
    );
    CREATE INDEX IF NOT EXISTS recipients_prefix ON recipients(prefix, sequence);
    CREATE INDEX IF NOT EXISTS recipients_address ON recipients(address, sequence);
";

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
    /// A letter that is not a format-1 letter the parser accepts (a legacy
    /// letter with no `X-Vivi-Format` included). The string is the
    /// refusal's message.
    LetterFormat(String),
    /// Storage or IO failure.
    Storage(String),
    /// A chat project is closed or past its TTL (item 17).
    Gone { closed: bool },
    /// The project refuses this write as read-only. No append path returns
    /// it now that a hosted project files member appends.
    ReadOnly(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub project_id: String,
    /// `chat` for an ephemeral lightweight room; absent on a durable project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closed: bool,
    /// The log epoch the vivi client binds pulled letters under: 1 to
    /// `u32::MAX`, minted once, never changed by the server. Absent in a
    /// `project.json` written before epochs; minted on first use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u32>,
    /// Set on a hosted project (see `hosted`): the host project root whose
    /// own `.vivi/` is this project's mailspace. The server's per-project
    /// data (transport index, identities, keys, this file) stays under
    /// `DATA_DIR/projects/<id>/`; the mailspace does not. Absent on a
    /// project the server owns end to end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailspace_root: Option<PathBuf>,
}

impl ProjectMeta {
    fn durable(project_id: String, epoch: u32) -> Self {
        Self {
            project_id,
            mode: None,
            expires_at: None,
            ttl_seconds: None,
            closed: false,
            epoch: Some(epoch),
            mailspace_root: None,
        }
    }

    /// A project mailspace hosted from `root`, with a fresh epoch.
    pub(crate) fn hosted(project_id: String, root: PathBuf) -> Result<Self, ProjectError> {
        Ok(Self {
            mailspace_root: Some(root),
            ..Self::durable(project_id, mint_epoch()?)
        })
    }

    /// True for a project served from a host mailspace.
    pub fn is_hosted(&self) -> bool {
        self.mailspace_root.is_some()
    }

    /// The mailspace directory: the host root's `.vivi` for a hosted
    /// project, else `.vivi` inside the server's own project directory.
    fn mailspace_dir(&self, project_dir: &Path) -> PathBuf {
        match &self.mailspace_root {
            Some(root) => root.join(MAILSPACE_DIR),
            None => project_dir.join(MAILSPACE_DIR),
        }
    }

    /// True for an item-17 lightweight chat project.
    pub fn is_chat(&self) -> bool {
        self.mode.as_deref() == Some("chat")
    }

    /// Closed, or past `expires_at`. Durable projects are never over.
    pub fn is_over(&self) -> bool {
        if !self.is_chat() {
            return false;
        }
        if self.closed {
            return true;
        }
        self.expires_at
            .as_deref()
            .and_then(|stamp| DateTime::parse_from_rfc3339(stamp).ok())
            .is_some_and(|stamp| stamp.with_timezone(&Utc) <= Utc::now())
    }
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
    /// The letter carries an `X-Vivi-Event` header: an item event letter,
    /// decided from the letter's own bytes and never from its envelope kind.
    event: bool,
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
            Some(k @ ("need" | "task" | "want" | "chat")) => Some(k.to_owned()),
            Some(other) => {
                return Err(ProjectError::BadRequest(format!(
                    "unsupported kind '{other}'; expected need, task, want, chat, or mail"
                )));
            }
        };
        let to = parse_to(to)?;
        let event = carries_event_header(&raw_eml);
        Ok(Self {
            id: id.to_owned(),
            from: from.to_owned(),
            from_prefix: None,
            to,
            kind,
            event,
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

    /// The mailspace folder this letter files under: the work folder for
    /// need/task/want, `inbox` for mail (as the client files a pulled letter).
    /// An item event letter is a record and not work: it files as mail, as
    /// the client files it, whatever kind the envelope carries.
    fn local_role(&self) -> &'static str {
        if self.event {
            return "inbox";
        }
        match self.kind.as_deref() {
            Some("need") => "needs",
            Some("task") => "tasks",
            Some("want") => "wants",
            _ => "inbox",
        }
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
    /// Server clock at append, RFC 3339 UTC to the second.
    pub appended_at: String,
    /// The exact bytes that were appended.
    pub raw_eml: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReadAfter {
    pub records: Vec<ProjectRecord>,
    /// Cursor for the last record returned, or the supplied cursor if empty.
    pub next_cursor: u64,
    /// True when more records match after `next_cursor`.
    pub has_more: bool,
    /// The project's log epoch, sent on every list response.
    pub epoch: u32,
}

/// Result of a handle lookup: the visible letters, ordered by sequence, and
/// the handles that matched nothing the caller may see.
#[derive(Debug, PartialEq, Eq)]
pub struct Lookup {
    pub records: Vec<ProjectRecord>,
    pub missing: Vec<String>,
}

/// Which visible letters [`ProjectStore::scan_visible`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scan {
    /// Every letter the viewer may see.
    All,
    /// Work items (any envelope kind) and event letters: what a state fold
    /// and a work listing need. Plain mail is neither read nor returned.
    WorkAndEvents,
}

/// Rows a scan takes from the transport index per query.
const SCAN_CHUNK: usize = 500;

/// Default page size of a list.
pub const DEFAULT_LIMIT: usize = 500;
/// Largest page size a list will honor.
pub const MAX_LIMIT: usize = 1000;
/// A page stops adding letters once their raw bytes exceed this.
pub const PAGE_BYTE_CAP: usize = 8 * 1024 * 1024;
/// Most handles one lookup may carry.
pub const MAX_HANDLES: usize = 100;
/// Characters in a handle: the hex after `msg_` in a message id.
const HANDLE_LEN: usize = 8;

/// Which recipient a `to` filter names.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToFilter {
    /// An exact address, lowercased.
    Address(String),
    /// A bare 8-hex prefix: any `<prefix>.local` address.
    Prefix(String),
}

/// A validated list request: the cursor plus optional narrowing filters.
/// Filters only ever narrow what visibility allows.
#[derive(Debug, Clone)]
pub struct ListQuery {
    since: u64,
    limit: usize,
    /// Lower bound on `appended_at`, normalized to whole seconds.
    after: Option<String>,
    /// Allowed kinds; `None` inside the list is plain mail.
    kinds: Option<Vec<Option<String>>>,
    to: Option<ToFilter>,
}

fn bad_param(message: &str) -> ProjectError {
    ProjectError::BadRequest(message.to_owned())
}

impl ListQuery {
    /// Only the cursor: default page size, no filters.
    pub fn since(since: u64) -> Self {
        Self {
            since,
            limit: DEFAULT_LIMIT,
            after: None,
            kinds: None,
            to: None,
        }
    }

    /// Parse the raw query parameters. A malformed `limit`, `after`,
    /// `kind`, or `to` is a `BadRequest`; a `limit` above the maximum is
    /// clamped to it.
    pub fn parse(
        since: u64,
        limit: Option<&str>,
        after: Option<&str>,
        kind: Option<&str>,
        to: Option<&str>,
    ) -> Result<Self, ProjectError> {
        let mut query = Self::since(since);
        if let Some(limit) = limit {
            let parsed: usize = limit
                .parse()
                .ok()
                .filter(|n| *n >= 1)
                .ok_or_else(|| bad_param("limit must be a positive integer"))?;
            query.limit = parsed.min(MAX_LIMIT);
        }
        query.after = after.map(parse_after).transpose()?;
        query.kinds = kind.map(parse_kinds).transpose()?;
        query.to = to.map(parse_to_filter).transpose()?;
        Ok(query)
    }

    /// Append this query's narrowing filters to a WHERE clause list, each
    /// with its bound parameters in order.
    fn add_filters(&self, clauses: &mut Vec<String>, params: &mut Vec<Value>) {
        if let Some(kinds) = &self.kinds {
            let named: Vec<&str> = kinds.iter().flatten().map(String::as_str).collect();
            let mut parts = Vec::new();
            if kinds.iter().any(Option::is_none) {
                parts.push("i.kind IS NULL".to_owned());
            }
            if !named.is_empty() {
                let marks = vec!["?"; named.len()].join(", ");
                parts.push(format!("i.kind IN ({marks})"));
                params.extend(named.iter().map(|k| Value::Text((*k).to_owned())));
            }
            clauses.push(format!("({})", parts.join(" OR ")));
        }
        if let Some(after) = &self.after {
            clauses.push("i.appended_at >= ?".to_owned());
            params.push(Value::Text(after.clone()));
        }
        match &self.to {
            None => {}
            Some(ToFilter::Address(address)) => {
                clauses.push(
                    "EXISTS (SELECT 1 FROM recipients r WHERE r.sequence = i.sequence \
                     AND r.address = ?)"
                        .to_owned(),
                );
                params.push(Value::Text(address.clone()));
            }
            Some(ToFilter::Prefix(prefix)) => {
                clauses.push(
                    "EXISTS (SELECT 1 FROM recipients r WHERE r.sequence = i.sequence \
                     AND r.prefix = ?)"
                        .to_owned(),
                );
                params.push(Value::Text(prefix.clone()));
            }
        }
    }
}

/// An RFC 3339 instant as a whole-second UTC string, rounded up so that
/// "at or after" never admits a row stamped before the requested instant.
fn parse_after(raw: &str) -> Result<String, ProjectError> {
    let parsed = DateTime::parse_from_rfc3339(raw.trim())
        .map_err(|_| bad_param("after must be an RFC 3339 time"))?
        .with_timezone(&Utc);
    let whole = parsed.with_nanosecond(0).unwrap_or(parsed);
    let whole = if parsed.nanosecond() > 0 {
        whole + chrono::Duration::seconds(1)
    } else {
        whole
    };
    Ok(whole.to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn parse_kinds(raw: &str) -> Result<Vec<Option<String>>, ProjectError> {
    raw.split(',')
        .map(|kind| match kind.trim().to_ascii_lowercase().as_str() {
            "mail" => Ok(None),
            k @ ("need" | "task" | "want" | "chat") => Ok(Some(k.to_owned())),
            _ => Err(bad_param("kind must list need, task, want, chat, or mail")),
        })
        .collect()
}

fn parse_to_filter(raw: &str) -> Result<ToFilter, ProjectError> {
    let raw = raw.trim();
    if raw.len() == MAILSPACE_PREFIX_LEN && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(ToFilter::Prefix(raw.to_ascii_lowercase()))
    } else if valid_address(raw) {
        Ok(ToFilter::Address(raw.to_ascii_lowercase()))
    } else {
        Err(bad_param(
            "to must be a local@domain address or an 8-hex mailspace prefix",
        ))
    }
}

/// The stamp for a new row: the server clock to the second, never behind
/// the previous row's stamp, so a clock step cannot reorder history.
pub fn monotone_stamp(previous: Option<&str>, now: DateTime<Utc>) -> String {
    let now = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    match previous {
        Some(previous) if previous > now.as_str() => previous.to_owned(),
        _ => now,
    }
}

/// One row of the transport index.
struct Row {
    sequence: u64,
    message_id: String,
    from: String,
    from_prefix: Option<String>,
    to: Option<String>,
    kind: Option<String>,
    content_sha256: String,
    byte_size: u64,
    appended_at: String,
}

/// The `items` columns [`Row`] reads, in order.
const COLUMNS: &str = "i.sequence, i.message_id, i.from_name, i.from_prefix, i.to_raw, i.kind, \
                       i.content_sha256, i.byte_size, i.appended_at";

fn row_of(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let sequence: i64 = row.get(0)?;
    let byte_size: i64 = row.get(7)?;
    Ok(Row {
        sequence: u64::try_from(sequence).unwrap_or(0),
        message_id: row.get(1)?,
        from: row.get(2)?,
        from_prefix: row.get(3)?,
        to: row.get(4)?,
        kind: row.get(5)?,
        content_sha256: row.get(6)?,
        byte_size: u64::try_from(byte_size).unwrap_or(0),
        appended_at: row.get(8)?,
    })
}

/// Scoped-read visibility (decided items 7 and 11 plus the governance
/// carve-out), decided from the transport row alone: work items, item event
/// letters, and mail addressed to postmaster are member-visible; every other
/// letter only to its sender (by name) or to the member whose registered
/// prefix is named by a recipient address. An event letter is decided by its
/// `X-Vivi-Event` header (the `event` column), not by its envelope kind: the
/// client sends it as `mail`, and every member that holds the item must see
/// it. Pushes the clause's parameters in order.
fn visible_clause(viewer: &Viewer<'_>, params: &mut Vec<Value>) -> String {
    let mut clause = String::from(
        "(i.kind IS NOT NULL OR i.event = 1 OR i.from_name = ? OR lower(i.to_raw) = 'postmaster'",
    );
    params.push(Value::Text(viewer.name.to_owned()));
    if let Some(prefix) = viewer.prefix {
        clause.push_str(
            " OR EXISTS (SELECT 1 FROM recipients r WHERE r.sequence = i.sequence \
             AND r.prefix = ?)",
        );
        params.push(Value::Text(prefix.to_ascii_lowercase()));
    }
    clause.push(')');
    clause
}

/// Whether the transport index has its `event` column yet.
fn has_event_column(transport: &Connection) -> Result<bool, ProjectError> {
    transport
        .prepare("SELECT 1 FROM pragma_table_info('items') WHERE name = 'event'")
        .and_then(|mut statement| statement.exists([]))
        .map_err(storage_err("failed to inspect the transport index"))
}

/// Bring an index created before the `event` column existed up to date: add
/// the column, then mark every held mail letter (no envelope kind) whose
/// bytes carry `X-Vivi-Event`, so such a letter is member-visible from the
/// first read after an upgrade. Runs once per index, under the write lock,
/// and re-checks under it so two openers racing past the first check do not
/// both alter the table.
fn ensure_event_column(transport: &Connection, storage: &Storage) -> Result<(), ProjectError> {
    if has_event_column(transport)? {
        return Ok(());
    }
    transport
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(storage_err("failed to lock the transport index"))?;
    match add_event_column(transport, storage) {
        Ok(()) => transport
            .execute_batch("COMMIT")
            .map_err(storage_err("failed to commit the transport migration")),
        Err(error) => {
            let _ = transport.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn add_event_column(transport: &Connection, storage: &Storage) -> Result<(), ProjectError> {
    if has_event_column(transport)? {
        return Ok(());
    }
    transport
        .execute_batch("ALTER TABLE items ADD COLUMN event INTEGER NOT NULL DEFAULT 0")
        .map_err(storage_err("failed to add the transport event column"))?;
    let mail: Vec<(i64, String)> = {
        let mut statement = transport
            .prepare("SELECT sequence, content_sha256 FROM items WHERE kind IS NULL")
            .map_err(storage_err("failed to prepare the event backfill"))?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .and_then(Iterator::collect)
            .map_err(storage_err("failed to read the event backfill"))?
    };
    for (sequence, content_sha256) in mail {
        // A letter the store cannot read is left unmarked: the read path
        // reports it when it is served, as it always has.
        let Ok(raw) = storage.read_blob(&content_sha256) else {
            continue;
        };
        if carries_event_header(&raw) {
            transport
                .execute(
                    "UPDATE items SET event = 1 WHERE sequence = ?1",
                    params![sequence],
                )
                .map_err(storage_err("failed to mark an event letter"))?;
        }
    }
    Ok(())
}

fn storage_err<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> ProjectError {
    move |error| ProjectError::Storage(format!("{context}: {error}"))
}

/// An open project: its transport index and its mailspace store.
struct Project {
    transport: Connection,
    storage: Storage,
}

impl Project {
    /// Open the transport index in `dir` and the mailspace at `mailspace_dir`
    /// (inside `dir` unless the project is hosted).
    fn open(dir: &Path, mailspace_dir: &Path) -> Result<Self, ProjectError> {
        let transport = Connection::open(dir.join(TRANSPORT_FILE))
            .map_err(storage_err("failed to open the transport index"))?;
        transport
            .busy_timeout(Duration::from_secs(5))
            .map_err(storage_err("failed to set the transport timeout"))?;
        transport
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(storage_err("failed to set the transport journal mode"))?;
        transport
            .execute_batch(TRANSPORT_SCHEMA)
            .map_err(storage_err("failed to prepare the transport index"))?;
        let storage = Storage::open_mailspace(mailspace_dir)
            .map_err(storage_err("failed to open the project mailspace"))?;
        ensure_event_column(&transport, &storage)?;
        Ok(Self { transport, storage })
    }

    fn select(&self, sql: &str, params: Vec<Value>) -> Result<Vec<Row>, ProjectError> {
        let mut statement = self
            .transport
            .prepare(sql)
            .map_err(storage_err("failed to prepare a transport query"))?;
        let rows = statement
            .query_map(params_from_iter(params), row_of)
            .map_err(storage_err("failed to run a transport query"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(storage_err("failed to read a transport row"))
    }

    /// The replay rules: the same id with the same bytes is a duplicate, the
    /// same id with other bytes conflicts, and an unseen id with bytes the
    /// SAME sender already holds under another id is a duplicate of that
    /// original. The last rule is sender-scoped on purpose: answering any
    /// member would let one who holds the exact bytes of mail they cannot see
    /// confirm it exists and learn its id and sequence.
    fn find_replay(
        &self,
        id: &str,
        from: &str,
        content_sha256: &str,
    ) -> Result<Option<AppendOutcome>, ProjectError> {
        let duplicate = |row: Row| AppendOutcome {
            message_id: row.message_id,
            sequence: row.sequence,
            content_sha256: row.content_sha256,
            duplicate: true,
        };
        let held = self.select(
            &format!("SELECT {COLUMNS} FROM items i WHERE i.message_id = ?"),
            vec![Value::Text(id.to_owned())],
        )?;
        if let Some(row) = held.into_iter().next() {
            if row.content_sha256 == content_sha256 {
                return Ok(Some(duplicate(row)));
            }
            return Err(ProjectError::Conflict(format!(
                "item id {id} already names a different letter in this project"
            )));
        }
        let same_bytes = self.select(
            &format!(
                "SELECT {COLUMNS} FROM items i WHERE i.from_name = ? AND i.content_sha256 = ? \
                 ORDER BY i.sequence LIMIT 1"
            ),
            vec![
                Value::Text(from.to_owned()),
                Value::Text(content_sha256.to_owned()),
            ],
        )?;
        Ok(same_bytes.into_iter().next().map(duplicate))
    }

    /// The next sequence and the previous row's stamp, read together.
    fn tail(&self) -> Result<(u64, Option<String>), ProjectError> {
        let last: Option<(i64, String)> = self
            .transport
            .query_row(
                "SELECT sequence, appended_at FROM items ORDER BY sequence DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(storage_err("failed to read the transport tail"))?;
        Ok(match last {
            Some((sequence, stamp)) => (u64::try_from(sequence).unwrap_or(0) + 1, Some(stamp)),
            None => (1, None),
        })
    }

    /// Ingest the letter verbatim into the project mailspace under its
    /// presented id. Content already held under this id is not an error.
    fn file_letter(&mut self, project_id: &str, item: &AppendItem) -> Result<(), ProjectError> {
        let request = MessageIngestRequest {
            account: project_id.to_owned(),
            local_role: item.local_role().to_owned(),
            read_state: false,
            starred: false,
            message_id_hint: None,
            presented_id: Some(item.id.clone()),
            seed_hint: String::new(),
            remote: None,
        };
        match self.storage.ingest_if_absent(&request, &item.raw_eml, None) {
            Ok((IngestOutcome::Conflict, _)) => Err(ProjectError::Conflict(format!(
                "item id {} already names a different letter in this project",
                item.id
            ))),
            Ok(_) => Ok(()),
            Err(error) => Err(ProjectError::Storage(format!(
                "failed to file the letter in the project mailspace: {error}"
            ))),
        }
    }

    /// Record the transport row and its recipients in one transaction.
    fn record(
        &mut self,
        item: &AppendItem,
        content_sha256: &str,
        sequence: u64,
        appended_at: &str,
    ) -> Result<(), ProjectError> {
        let tx = self
            .transport
            .transaction()
            .map_err(storage_err("failed to open a transport transaction"))?;
        let handle = handle_of(&item.id).unwrap_or_default();
        tx.execute(
            "INSERT INTO items (sequence, message_id, handle, content_sha256, byte_size, \
             from_name, from_prefix, to_raw, kind, appended_at, event) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                i64::try_from(sequence).unwrap_or(i64::MAX),
                item.id,
                handle,
                content_sha256,
                i64::try_from(item.raw_eml.len()).unwrap_or(i64::MAX),
                item.from,
                item.from_prefix,
                item.to,
                item.kind,
                appended_at,
                item.event,
            ],
        )
        .map_err(storage_err("failed to record the letter's transport row"))?;
        for address in item.to.iter().flat_map(|to| recipients(to)) {
            let prefix = address
                .split_once('@')
                .and_then(|(_, domain)| domain_prefix(domain))
                .map(str::to_ascii_lowercase);
            tx.execute(
                "INSERT OR IGNORE INTO recipients (sequence, address, prefix) VALUES (?1, ?2, ?3)",
                params![
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    address.to_ascii_lowercase(),
                    prefix,
                ],
            )
            .map_err(storage_err("failed to record a recipient"))?;
        }
        tx.commit()
            .map_err(storage_err("failed to commit the transport row"))
    }

    /// The exact bytes of a row's letter, checked against its content hash.
    /// A missing or mismatched blob is a storage failure: never serve a
    /// sequence the store cannot back.
    fn letter(&self, row: &Row) -> Result<Vec<u8>, ProjectError> {
        let raw_eml = self.storage.read_blob(&row.content_sha256).map_err(|e| {
            ProjectError::Storage(format!(
                "sequence {} names letter {} that cannot be read: {e}",
                row.sequence, row.content_sha256
            ))
        })?;
        if sha256_hex(&raw_eml) != row.content_sha256 {
            return Err(ProjectError::Storage(format!(
                "letter blob for sequence {} does not match its content hash",
                row.sequence
            )));
        }
        Ok(raw_eml)
    }

    fn record_of(&self, row: Row) -> Result<ProjectRecord, ProjectError> {
        let raw_eml = self.letter(&row)?;
        Ok(ProjectRecord {
            sequence: row.sequence,
            message_id: row.message_id,
            from: row.from,
            from_prefix: row.from_prefix,
            to: row.to,
            kind: row.kind,
            content_sha256: row.content_sha256,
            appended_at: row.appended_at,
            raw_eml,
        })
    }
}

/// Per-project storage root under `DATA_DIR/projects`; project metadata
/// persists in `project.json` beside the mailspace and the transport index.
#[derive(Debug, Clone)]
pub struct ProjectStore {
    projects_root: PathBuf,
    admissions: AdmissionRegistry,
    wake: Arc<ProjectWake>,
    clock: Clock,
    /// Ids of hosted projects whose root the registry no longer serves: the
    /// data stays on disk, the project answers as unknown.
    retired: Arc<RwLock<HashSet<String>>>,
    /// Registered hosted projects: their indexed sender and pass state.
    hosted: hosted_index::HostedMap,
}

/// The server clock the append stamp reads; injectable so tests can step it.
#[derive(Clone)]
struct Clock(Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>);

impl std::fmt::Debug for Clock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Clock")
    }
}

/// A fresh epoch: random, 1 to `u32::MAX` (0 means "no epoch" to a client).
fn mint_epoch() -> Result<u32, ProjectError> {
    loop {
        let mut raw = [0u8; 4];
        getrandom::fill(&mut raw)
            .map_err(|_| ProjectError::Storage("entropy source unavailable".into()))?;
        let epoch = u32::from_be_bytes(raw);
        if epoch != 0 {
            return Ok(epoch);
        }
    }
}

fn meta_lock() -> MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn append_lock() -> MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether the letter's header block carries an `X-Vivi-Event` header: an
/// item event letter (`done`, `reopened`, `absorbed`, `marked`, `unmarked`,
/// `bound`, or a kind a later client adds). Only the header name counts;
/// `X-Vivi-Event-Of` and the other payload headers are other names.
fn carries_event_header(raw: &[u8]) -> bool {
    for line in raw.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            return false;
        }
        if let Some(colon) = line.iter().position(|byte| *byte == b':')
            && line[..colon]
                .trim_ascii_end()
                .eq_ignore_ascii_case(b"x-vivi-event")
        {
            return true;
        }
    }
    false
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

/// A letter the server files must be format 1 and must parse. One with no
/// `X-Vivi-Format` header is legacy and is refused too: the server accepts
/// only format-1 letters (W3-B4; vivi 21 writes no legacy letter anymore).
fn check_letter_format(raw: &[u8]) -> Result<(), ProjectError> {
    match of_message(raw, "") {
        Ok(_) => Ok(()),
        Err(LetterError::Legacy { .. }) => Err(ProjectError::LetterFormat(
            "the server accepts only format-1 letters: this one has no X-Vivi-Format header"
                .to_owned(),
        )),
        Err(error) => Err(ProjectError::LetterFormat(error.to_string())),
    }
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
        Self::with_clock(data_dir, Utc::now)
    }

    /// A store whose append stamp reads `clock` instead of the system clock.
    pub fn with_clock(
        data_dir: &Path,
        clock: impl Fn() -> DateTime<Utc> + Send + Sync + 'static,
    ) -> Self {
        Self {
            projects_root: data_dir.join("projects"),
            admissions: AdmissionRegistry::new(data_dir),
            wake: Arc::new(ProjectWake::default()),
            clock: Clock(Arc::new(clock)),
            retired: Arc::default(),
            hosted: Arc::default(),
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

    /// Mint a collision-retried project id and create its mailspace and
    /// transport index.
    pub fn create_project(&self) -> Result<ProjectMeta, ProjectError> {
        for _ in 0..8 {
            let project_id = mint_room_id()
                .map_err(|_| ProjectError::Storage("entropy source unavailable".into()))?;
            let dir = self.projects_root.join(&project_id);
            if dir.exists() {
                continue;
            }
            Mailspace::init(Some(&dir))
                .map_err(storage_err("failed to create the project mailspace"))?;
            let epoch = mint_epoch()?;
            let meta = ProjectMeta::durable(project_id, epoch);
            let bytes = serde_json::to_vec(&meta).map_err(|e| {
                ProjectError::Storage(format!("failed to encode project.json: {e}"))
            })?;
            std::fs::write(dir.join("project.json"), &bytes)
                .map_err(|e| ProjectError::Storage(format!("failed to write project.json: {e}")))?;
            // Prepare the transport index now so a project is fully formed
            // before its first append.
            Project::open(&dir, &dir.join(MAILSPACE_DIR))?;
            // Project files (project.json, the mailspace, the transport
            // index) stay owner-only, like the room tree (VC-5).
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
        if !dir.join("project.json").is_file() {
            return Err(ProjectError::ProjectNotFound);
        }
        if let Some(id) = dir.file_name().and_then(|n| n.to_str())
            && self
                .retired
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains(id)
        {
            return Err(ProjectError::ProjectNotFound);
        }
        Ok(dir)
    }

    /// Open an existing project: its transport index and its mailspace,
    /// wherever `project.json` says the mailspace lives.
    fn open_project(&self, raw_id: &str) -> Result<Project, ProjectError> {
        let dir = self.existing_project_dir(raw_id)?;
        let meta = self.load_meta(raw_id)?;
        let mailspace_dir = meta.mailspace_dir(&dir);
        if meta.is_hosted() && !mailspace_dir.join("mailspace.toml").is_file() {
            return Err(ProjectError::Storage(format!(
                "hosted mailspace {} is missing",
                mailspace_dir.display()
            )));
        }
        Project::open(&dir, &mailspace_dir)
    }

    /// The mailspace directory of an existing project.
    pub fn mailspace_dir(&self, raw_id: &str) -> Result<PathBuf, ProjectError> {
        let dir = self.existing_project_dir(raw_id)?;
        Ok(self.load_meta(raw_id)?.mailspace_dir(&dir))
    }

    /// Prepare a hosted project's transport index in `dir` and prove its
    /// host mailspace opens. Independent of whether the id is retired, so it
    /// can run while a project is being (re)served.
    pub(crate) fn prepare_hosted(&self, dir: &Path, root: &Path) -> Result<(), ProjectError> {
        Project::open(dir, &root.join(MAILSPACE_DIR)).map(|_| ())
    }

    /// Replace the set of retired hosted ids (see `hosted`).
    pub(crate) fn set_retired(&self, retired: HashSet<String>) {
        *self.retired.write().unwrap_or_else(|e| e.into_inner()) = retired;
    }

    /// The directory holding every project's data.
    pub(crate) fn projects_root(&self) -> &Path {
        &self.projects_root
    }

    /// Load `project.json` for an existing project.
    pub fn load_meta(&self, raw_id: &str) -> Result<ProjectMeta, ProjectError> {
        let dir = self.existing_project_dir(raw_id)?;
        let bytes =
            std::fs::read(dir.join("project.json")).map_err(|_| ProjectError::ProjectNotFound)?;
        serde_json::from_slice(&bytes).map_err(|e| ProjectError::Storage(e.to_string()))
    }

    /// The project's log epoch. A project without one (created before
    /// epochs) gets one minted and persisted here, once; every later call
    /// and every restart returns the same value.
    pub fn epoch(&self, raw_id: &str) -> Result<u32, ProjectError> {
        if let Some(epoch) = self.load_meta(raw_id)?.epoch {
            return Ok(epoch);
        }
        let _guard = meta_lock();
        let mut meta = self.load_meta(raw_id)?;
        if let Some(epoch) = meta.epoch {
            return Ok(epoch);
        }
        let epoch = mint_epoch()?;
        meta.epoch = Some(epoch);
        self.write_meta(&meta)?;
        Ok(epoch)
    }

    fn write_meta(&self, meta: &ProjectMeta) -> Result<(), ProjectError> {
        let dir = self.existing_project_dir(&meta.project_id)?;
        let bytes = serde_json::to_vec(meta)
            .map_err(|e| ProjectError::Storage(format!("failed to encode project.json: {e}")))?;
        std::fs::write(dir.join("project.json"), &bytes)
            .map_err(|e| ProjectError::Storage(format!("failed to write project.json: {e}")))
    }

    /// Mint a chat-mode project (item 17): same store as a durable project,
    /// plus a TTL. The id is the shareable capability.
    pub fn create_chat_project(&self, ttl_seconds: u64) -> Result<ProjectMeta, ProjectError> {
        let meta = self.create_project()?;
        let expires_at = Utc::now() + chrono::Duration::seconds(ttl_seconds as i64);
        let meta = ProjectMeta {
            project_id: meta.project_id,
            mode: Some("chat".into()),
            expires_at: Some(expires_at.to_rfc3339_opts(SecondsFormat::Secs, true)),
            ttl_seconds: Some(ttl_seconds),
            closed: false,
            epoch: meta.epoch,
            mailspace_root: None,
        };
        self.write_meta(&meta)?;
        Ok(meta)
    }

    /// Explicit close: further joins and writes are gone. The directory
    /// stays until sweep.
    pub fn close_chat(&self, raw_id: &str) -> Result<ProjectMeta, ProjectError> {
        let mut meta = self.load_meta(raw_id)?;
        if !meta.is_chat() {
            return Err(ProjectError::BadRequest(
                "only a chat project can be closed".into(),
            ));
        }
        meta.closed = true;
        self.write_meta(&meta)?;
        Ok(meta)
    }

    /// Refuse writes (and chat reads that require a live channel) when the
    /// project is a closed or expired chat. Durable projects always pass.
    pub fn refuse_if_chat_over(&self, raw_id: &str) -> Result<ProjectMeta, ProjectError> {
        let meta = self.load_meta(raw_id)?;
        if meta.is_over() {
            return Err(ProjectError::Gone {
                closed: meta.closed,
            });
        }
        Ok(meta)
    }

    /// Delete closed or expired chat project directories. Durable projects
    /// are never touched. Returns how many trees were removed.
    pub fn sweep_expired_chats(&self) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.projects_root) else {
            return 0;
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            let dir = entry.path();
            let Ok(bytes) = std::fs::read(dir.join("project.json")) else {
                continue;
            };
            let Ok(meta) = serde_json::from_slice::<ProjectMeta>(&bytes) else {
                continue;
            };
            if meta.is_over() && std::fs::remove_dir_all(&dir).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    /// Live (not yet over) chat projects, for `/v1/_health`.
    pub fn live_chat_count(&self) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.projects_root) else {
            return 0;
        };
        entries
            .flatten()
            .filter(|entry| {
                std::fs::read(entry.path().join("project.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<ProjectMeta>(&bytes).ok())
                    .is_some_and(|meta| meta.is_chat() && !meta.is_over())
            })
            .count()
    }

    /// Storage state for `/v1/_health`.
    pub fn storage_state(&self) -> String {
        std::fs::create_dir_all(&self.projects_root)
            .map(|_| "ok".to_owned())
            .unwrap_or_else(|e| format!("error: {e}"))
    }

    /// Process the governance vouch in a letter to postmaster: validate it
    /// and write the admission. Idempotent, so a replay after a crash
    /// between this and the transport write converges.
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
    /// [`Project::find_replay`]; a fresh letter is filed verbatim in the
    /// project mailspace first, then recorded as the next transport row.
    /// A fresh letter that is not a format-1 letter the parser accepts
    /// (a legacy letter included) is refused before that write. Hosted
    /// projects return earlier and are not checked here.
    pub fn append(&self, raw_id: &str, item: &AppendItem) -> Result<AppendOutcome, ProjectError> {
        let _guard = append_lock();
        let meta = self.refuse_if_chat_over(raw_id)?;
        if meta.is_hosted() {
            return self.append_hosted(raw_id, &meta, item);
        }
        let dir = self.existing_project_dir(raw_id)?;
        let project_id = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        let content_sha256 = sha256_hex(&item.raw_eml);
        let mut project = self.open_project(raw_id)?;
        if let Some(outcome) = project.find_replay(&item.id, &item.from, &content_sha256)? {
            return Ok(outcome);
        }
        // Replay keeps its outcome, so only a fresh letter is judged, and
        // the check runs before the vouch so a refusal admits nobody.
        check_letter_format(&item.raw_eml)?;
        // Governance vouch (decided item 8): the admission is written FIRST,
        // then the letter is stored; both are idempotent, so a crash between
        // them converges on replay.
        if item.is_governance() {
            self.process_vouch(&project_id, item)?;
        }
        let (sequence, previous) = project.tail()?;
        let appended_at = monotone_stamp(previous.as_deref(), (self.clock.0)());
        project.file_letter(&project_id, item)?;
        project.record(item, &content_sha256, sequence, &appended_at)?;
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

    /// Read letters after a transport cursor, scoped to `viewer`, with the
    /// default page size and no filters. See [`read_page`](Self::read_page).
    pub fn read_after_scoped(
        &self,
        raw_id: &str,
        since: u64,
        viewer: &Viewer<'_>,
    ) -> Result<ReadAfter, ProjectError> {
        self.read_page(raw_id, &ListQuery::since(since), viewer)
    }

    /// Read one page of letters scoped to `viewer` (decided items 7, 11, 13
    /// plus the governance carve-out): work items (need/task/want), item
    /// event letters (by their `X-Vivi-Event` header, whatever the envelope
    /// kind) and mail addressed to postmaster are member-visible; every
    /// other letter only to its sender or the recipient prefix's member. Visibility and the
    /// query's filters are applied in the transport query before the page
    /// limit, so a letter the viewer cannot see or the filters exclude is
    /// never read and never makes a page short. A page ends at the limit or
    /// once its raw letters exceed [`PAGE_BYTE_CAP`], whichever comes first,
    /// and always holds at least one record when any match. `next_cursor`
    /// is the last returned sequence, or the query's `since`.
    pub fn read_page(
        &self,
        raw_id: &str,
        query: &ListQuery,
        viewer: &Viewer<'_>,
    ) -> Result<ReadAfter, ProjectError> {
        let epoch = self.epoch(raw_id)?;
        let project = self.open_project(raw_id)?;
        self.refresh_hosted(raw_id, &project);
        let mut params = vec![Value::Integer(
            i64::try_from(query.since).unwrap_or(i64::MAX),
        )];
        let mut clauses = vec!["i.sequence > ?".to_owned()];
        clauses.push(visible_clause(viewer, &mut params));
        query.add_filters(&mut clauses, &mut params);
        let sql = format!(
            "SELECT {COLUMNS} FROM items i WHERE {} ORDER BY i.sequence LIMIT {}",
            clauses.join(" AND "),
            query.limit + 1
        );
        let rows = project.select(&sql, params)?;
        let matched = rows.len();
        let mut records = Vec::with_capacity(matched.min(query.limit));
        let mut next_cursor = query.since;
        let mut bytes = 0u64;
        for row in rows {
            if records.len() >= query.limit || (bytes > PAGE_BYTE_CAP as u64 && !records.is_empty())
            {
                break;
            }
            bytes += row.byte_size;
            next_cursor = row.sequence;
            records.push(project.record_of(row)?);
        }
        let has_more = records.len() < matched;
        Ok(ReadAfter {
            records,
            next_cursor,
            has_more,
            epoch,
        })
    }

    /// Visit every letter `viewer` may see, in append order, handing each to
    /// `visit` as it is read and keeping none: the caller keeps what it needs,
    /// so a fold over a project of any size holds only its own result. The
    /// same visibility as [`read_page`](Self::read_page), with no page
    /// limit and no byte cap: the transport index is walked by cursor in
    /// chunks of [`SCAN_CHUNK`] until it is exhausted, so a letter past the
    /// first page is seen. A letter the store cannot back is a storage
    /// failure, as on a page.
    pub fn scan_visible<F>(
        &self,
        raw_id: &str,
        viewer: &Viewer<'_>,
        scan: Scan,
        visit: F,
    ) -> Result<(), ProjectError>
    where
        F: FnMut(ProjectRecord),
    {
        self.scan_visible_in_chunks(raw_id, viewer, scan, SCAN_CHUNK, visit)
    }

    /// [`scan_visible`](Self::scan_visible) taking `chunk` rows (at least
    /// one) from the index per query. The result does not depend on the
    /// chunk; it is a parameter so a test can cross chunk boundaries with a
    /// handful of letters.
    pub fn scan_visible_in_chunks<F>(
        &self,
        raw_id: &str,
        viewer: &Viewer<'_>,
        scan: Scan,
        chunk: usize,
        mut visit: F,
    ) -> Result<(), ProjectError>
    where
        F: FnMut(ProjectRecord),
    {
        let chunk = chunk.max(1);
        let project = self.open_project(raw_id)?;
        self.refresh_hosted(raw_id, &project);
        let mut cursor = 0i64;
        loop {
            let mut params = vec![Value::Integer(cursor)];
            let mut clauses = vec!["i.sequence > ?".to_owned()];
            clauses.push(visible_clause(viewer, &mut params));
            if scan == Scan::WorkAndEvents {
                clauses.push("(i.kind IS NOT NULL OR i.event = 1)".to_owned());
            }
            let sql = format!(
                "SELECT {COLUMNS} FROM items i WHERE {} ORDER BY i.sequence LIMIT {chunk}",
                clauses.join(" AND ")
            );
            let rows = project.select(&sql, params)?;
            let last = rows.last().map(|row| row.sequence);
            let full = rows.len() == chunk;
            for row in rows {
                visit(project.record_of(row)?);
            }
            match last {
                Some(sequence) if full => {
                    cursor = i64::try_from(sequence).unwrap_or(i64::MAX);
                }
                _ => return Ok(()),
            }
        }
    }

    /// Look letters up by handle (the 8 hex after `msg_`), scoped to
    /// `viewer` by the same rule as the list. A handle may name several
    /// ids; one that names nothing visible is `missing`, identically for an
    /// unknown and a hidden id. Only matching, visible letters are read.
    pub fn lookup_handles(
        &self,
        raw_id: &str,
        handles: &[String],
        viewer: &Viewer<'_>,
    ) -> Result<Lookup, ProjectError> {
        let wanted = parse_handles(handles)?;
        let project = self.open_project(raw_id)?;
        self.refresh_hosted(raw_id, &project);
        let mut params: Vec<Value> = wanted.iter().map(|h| Value::Text(h.clone())).collect();
        let marks = vec!["?"; wanted.len()].join(", ");
        let visible = visible_clause(viewer, &mut params);
        let sql = format!(
            "SELECT {COLUMNS} FROM items i WHERE i.handle IN ({marks}) AND {visible} \
             ORDER BY i.sequence"
        );
        let rows = project.select(&sql, params)?;
        let mut found = HashSet::new();
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            if let Some(handle) = handle_of(&row.message_id) {
                found.insert(handle.to_owned());
            }
            records.push(project.record_of(row)?);
        }
        let mut seen = HashSet::new();
        let missing = handles
            .iter()
            .filter(|h| {
                !found.contains(&h.to_ascii_lowercase()) && seen.insert(h.to_ascii_lowercase())
            })
            .cloned()
            .collect();
        Ok(Lookup { records, missing })
    }

    /// Read one letter by id, scoped to `viewer` with the same rules as
    /// [`read_after_scoped`](Self::read_after_scoped). A letter the viewer
    /// may not see is `None`, exactly like an unknown id, and is decided
    /// from the transport row so it is never read.
    pub fn read_one_scoped(
        &self,
        raw_id: &str,
        message_id: &str,
        viewer: &Viewer<'_>,
    ) -> Result<Option<ProjectRecord>, ProjectError> {
        let project = self.open_project(raw_id)?;
        self.refresh_hosted(raw_id, &project);
        let mut params = vec![Value::Text(message_id.to_owned())];
        let visible = visible_clause(viewer, &mut params);
        let sql = format!("SELECT {COLUMNS} FROM items i WHERE i.message_id = ? AND {visible}");
        let Some(row) = project.select(&sql, params)?.into_iter().next() else {
            return Ok(None);
        };
        project.record_of(row).map(Some)
    }
}

/// The handle of a stored message id: the 8 hex after `msg_`.
fn handle_of(message_id: &str) -> Option<&str> {
    message_id.get(4..4 + HANDLE_LEN)
}

/// Validate a lookup's handles: 1 to [`MAX_HANDLES`], each 8 hex characters,
/// compared case-insensitively (returned lowercased).
fn parse_handles(handles: &[String]) -> Result<HashSet<String>, ProjectError> {
    if handles.is_empty() || handles.len() > MAX_HANDLES {
        return Err(ProjectError::BadRequest(format!(
            "handles must hold 1 to {MAX_HANDLES} entries"
        )));
    }
    handles
        .iter()
        .map(|handle| {
            if handle.len() == HANDLE_LEN && handle.bytes().all(|b| b.is_ascii_hexdigit()) {
                Ok(handle.to_ascii_lowercase())
            } else {
                Err(ProjectError::BadRequest(
                    "each handle must be 8 hex characters".to_owned(),
                ))
            }
        })
        .collect()
}
