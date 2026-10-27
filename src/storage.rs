use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::error::VivariumError;
use crate::store::secure_create_dir_all;

mod backlog_graph;
pub use backlog_graph::{
    BACKLOG_GRAPH_CODE, BacklogMintCommit, BacklogMintInput, BacklogNodeInput,
};
mod event_deferrals;
mod events;
mod goals;
mod graph;
mod handles;
mod ingest;
mod item_metadata;
mod links;
mod metadata;
mod mutate;
mod pending_deps;
mod query;
#[cfg(test)]
mod read_only_test;
pub(crate) mod schema;
mod sync;
#[cfg(test)]
mod tests;
mod watermark;

pub use goals::{GoalRow, goal_handle_for_path};
pub use graph::{
    WorkGraphActivateInput, WorkGraphApplyPlan, WorkGraphAttemptRow, WorkGraphEdgeInput,
    WorkGraphEdgeRow, WorkGraphEventRow, WorkGraphImportCommit, WorkGraphImportInput,
    WorkGraphNodeInput, WorkGraphNodeRow, WorkGraphRow, edge_handle_for, graph_handle_for_code,
    node_handle_for,
};
pub use links::MailspaceLink;
use metadata::parse_metadata;
pub use mutate::{ItemEventWrite, MailspaceMoveWithReply, MailspaceNoteWithReply, RoleMove};
pub use pending_deps::PendingDependencies;
use schema::{ensure_schema, message_query};
pub use sync::{IngestEvent, IngestOutcome};
pub use watermark::ChangeWatermark;

const MAILSPACE_DB_FILENAME: &str = "mail.sqlite";
const BLOBS_DIR: &str = "blobs";

/// Maximum bound parameters per batch-load statement. SQLite refuses to
/// prepare a statement with more parameters than its host limit (32,766 in
/// the bundled build), so id lists are queried in chunks of this size.
const SQL_PARAMETER_CHUNK: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    pub message_id: String,
    pub content_id: String,
    pub blob_relpath: String,
    pub created_blob: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageIngestRequest {
    pub account: String,
    pub local_role: String,
    pub read_state: bool,
    pub starred: bool,
    /// Trusted local override: the caller already owns this exact id
    /// (mailspace merge, drafts). Unvalidated; wins over every other source.
    pub message_id_hint: Option<String>,
    /// A traveling id presented by another store or the creator. Foreign
    /// input: it must pass [`is_traveling_id`] and wins over the
    /// namespace-seeded mints below, so every store files the item under
    /// the same id (and therefore the same short handle).
    pub presented_id: Option<String>,
    pub seed_hint: String,
    pub remote: Option<RemoteBindingInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBindingInput {
    pub account: String,
    pub provider: String,
    pub remote_mailbox: String,
    pub remote_uid: u32,
    pub remote_uidvalidity: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessageView {
    pub handle: String,
    pub message_id: String,
    pub account: String,
    pub content_id: String,
    pub blob_relpath: String,
    pub byte_size: u64,
    pub local_role: String,
    pub read_state: bool,
    pub starred: bool,
    pub date: String,
    pub from_addr: String,
    pub to_addr: String,
    pub cc_addr: String,
    pub bcc_addr: String,
    pub subject: String,
    pub normalized_message_id: Option<String>,
    pub remote: Option<RemoteBindingInput>,
    pub absorbed_at: Option<String>,
    pub absorbed_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailspaceEventInput {
    pub command: String,
    pub event_type: String,
    pub actor_identity: Option<String>,
    pub account: String,
    pub message_id: String,
    pub content_id: String,
    pub from_role: Option<String>,
    pub to_role: Option<String>,
    pub from_identity: Option<String>,
    pub to_identity: Option<String>,
    pub subject: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MailspaceEvent {
    pub event_id: i64,
    pub occurred_at: String,
    pub command: String,
    pub event_type: String,
    pub actor_identity: Option<String>,
    pub account: String,
    pub message_id: String,
    pub content_id: String,
    pub from_role: Option<String>,
    pub to_role: Option<String>,
    pub from_identity: Option<String>,
    pub to_identity: Option<String>,
    pub subject: String,
    pub note: Option<String>,
}

/// Whether a read-only open may treat the database owner as permitted to
/// trigger sidecar creation.
///
/// A read-only SQLite open never writes rows, but SQLite still maps the WAL
/// index through a `<database>-shm` sidecar and will create that sidecar
/// (plus `<database>-wal`) when it is missing and the process can write the
/// directory. That is a filesystem write the reader did not announce, so the
/// policy decides when it is tolerated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadOnlyWalPolicy {
    /// Only open when the `-shm` sidecar already exists. A WAL-mode database
    /// with no sidecar is refused instead of opened.
    ExistingOnly,
    /// Additionally allow the open when the reader's effective uid is proven
    /// equal to the uid that owns the database file. Unknown identity or
    /// unknown ownership never grants permission.
    AllowDatabaseOwner,
}

pub struct Storage {
    mail_root: PathBuf,
    conn: RefCell<Connection>,
    /// Cached handle index, cleared on any local write and rebuilt when the
    /// `PRAGMA data_version` freshness check in `handle_index` detects a
    /// commit from another connection.
    handle_index: RefCell<Option<handles::HandleIndex>>,
}

impl Storage {
    /// Open a mailspace-local storage database.
    ///
    /// Creates the mailspace directory and database file if they do not
    /// exist.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the directory cannot be created, the
    /// database cannot be opened, or the schema cannot be initialized.
    pub fn open_mailspace(mailspace_dir: &Path) -> Result<Self, VivariumError> {
        secure_create_dir_all(mailspace_dir)
            .map_err(|e| VivariumError::Other(format!("failed to create mailspace dir: {e}")))?;
        Self::open_with_db(mailspace_dir, &mailspace_dir.join(MAILSPACE_DB_FILENAME))
    }

    /// Open an existing mailspace database without creating the database or
    /// migrating its schema. The connection is opened with plain
    /// `SQLITE_OPEN_READ_ONLY` (never `immutable=1`): reads work as usual,
    /// any attempted write is rejected by SQLite, and no migration,
    /// initialization, checkpoint request, or application write happens on
    /// this path. What the open may still do is create the WAL sidecars when
    /// they are missing; [`ReadOnlyWalPolicy`] decides whether that is
    /// tolerated. [`open_mailspace_read_only_with_policy`] selects a policy;
    /// this form keeps the strict [`ReadOnlyWalPolicy::ExistingOnly`]
    /// behavior.
    ///
    /// # Errors
    /// Returns an error when the database is missing, unreadable, or the
    /// connection cannot be configured.
    pub fn open_mailspace_read_only(mailspace_dir: &Path) -> Result<Self, VivariumError> {
        Self::open_mailspace_read_only_with_policy(mailspace_dir, ReadOnlyWalPolicy::ExistingOnly)
    }

    /// Crate-visible form of [`Storage::open_mailspace_read_only`] that
    /// names the WAL sidecar policy.
    ///
    /// # Errors
    /// Returns an error when the database is missing, unreadable, the policy
    /// refuses the WAL state, or the connection cannot be configured.
    pub(crate) fn open_mailspace_read_only_with_policy(
        mailspace_dir: &Path,
        policy: ReadOnlyWalPolicy,
    ) -> Result<Self, VivariumError> {
        let conn = Self::open_mailspace_connection_read_only(mailspace_dir, policy)?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|e| VivariumError::Other(format!("failed to set SQLite timeout: {e}")))?;
        Ok(Self {
            mail_root: mailspace_dir.to_path_buf(),
            conn: RefCell::new(conn),
            handle_index: RefCell::new(None),
        })
    }

    /// Open the database connection read-only after checking that a WAL
    /// shared-memory sidecar already exists when the database needs one.
    /// Readers that need custom queries (such as the monitor) use this same
    /// guard instead of opening SQLite directly.
    ///
    /// # Errors
    /// Returns an error when the database is missing, unreadable, or would
    /// require creating the WAL shared-memory sidecar.
    pub(crate) fn open_mailspace_connection_read_only(
        mailspace_dir: &Path,
        policy: ReadOnlyWalPolicy,
    ) -> Result<Connection, VivariumError> {
        let db_path = mailspace_dir.join(MAILSPACE_DB_FILENAME);
        if read_only_wal_index_refused(&db_path, policy, effective_reader_uid()) {
            return Err(VivariumError::Other(format!(
                "read-only inspection unavailable: the WAL-mode mail store has no {MAILSPACE_DB_FILENAME}-shm sidecar; refusing to create it",
            )));
        }
        Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| {
            VivariumError::Other(format!("failed to open storage database read-only: {e}"))
        })
    }

    /// True when a read-only WAL open would need to create the missing
    /// shared-memory index beside the database. Owner-blind: callers such as
    /// the doctor only want the strict existence answer.
    #[must_use]
    pub(crate) fn read_only_wal_index_missing(mailspace_dir: &Path) -> bool {
        read_only_wal_index_refused(
            &mailspace_dir.join(MAILSPACE_DB_FILENAME),
            ReadOnlyWalPolicy::ExistingOnly,
            None,
        )
    }

    fn open_with_db(mail_root: &Path, db_path: &Path) -> Result<Self, VivariumError> {
        secure_create_dir_all(&mail_root.join(BLOBS_DIR))
            .map_err(|e| VivariumError::Other(format!("failed to create blob dir: {e}")))?;

        let mut conn = Connection::open(db_path)
            .map_err(|e| VivariumError::Other(format!("failed to open storage database: {e}")))?;
        // A deferred transaction that reads and then writes fails at once with
        // `database is locked` when another connection committed in between;
        // the busy timeout never applies. Taking the write lock up front lets
        // the timeout do its job.
        conn.set_transaction_behavior(TransactionBehavior::Immediate);
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|e| VivariumError::Other(format!("failed to set SQLite timeout: {e}")))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| VivariumError::Other(format!("failed to set WAL journal mode: {e}")))?;
        #[cfg(unix)]
        fs::set_permissions(db_path, fs::Permissions::from_mode(0o600))?;

        ensure_schema(&conn)?;

        Ok(Self {
            mail_root: mail_root.to_path_buf(),
            conn: RefCell::new(conn),
            handle_index: RefCell::new(None),
        })
    }

    /// Clear the cached handle index after any write that affects messages.
    fn invalidate_handle_index(&self) {
        *self.handle_index.borrow_mut() = None;
    }
}

// POSIX `geteuid(2)` on macOS, declared locally so only the Linux path
// depends on the `libc` crate's binding.
#[cfg(target_os = "macos")]
unsafe extern "C" {
    /// Effective user id of the calling process, per POSIX `geteuid(2)`.
    fn geteuid() -> u32;
}

/// Effective user id of this process, when the platform can prove it.
///
/// Linux reads `libc::geteuid`; macOS declares the same POSIX call locally;
/// other platforms report unknown identity rather than guessing.
#[must_use]
pub(crate) fn effective_reader_uid() -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `geteuid` takes no arguments, touches no memory of ours,
        // and cannot fail.
        Some(unsafe { libc::geteuid() })
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: `geteuid` takes no arguments, touches no memory of ours,
        // and cannot fail; the declaration matches the POSIX prototype.
        Some(unsafe { geteuid() })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Whether a read-only open of `db_path` under `policy` is refused because
/// it would need to create the missing WAL shared-memory sidecar.
///
/// Existing sidecars and non-WAL databases never refuse. A WAL-mode database
/// with no sidecar refuses under [`ReadOnlyWalPolicy::ExistingOnly`]; under
/// [`ReadOnlyWalPolicy::AllowDatabaseOwner`] it refuses unless the supplied
/// `reader_uid` is proven equal to the database file owner. Unknown identity
/// or unknown ownership never grants permission. Evaluation only reads: no
/// sidecar is ever created here.
#[must_use]
pub(crate) fn read_only_wal_index_refused(
    db_path: &Path,
    policy: ReadOnlyWalPolicy,
    reader_uid: Option<u32>,
) -> bool {
    if wal_index_sidecar_path(db_path).exists() {
        return false;
    }
    if !database_header_is_wal(db_path) {
        return false;
    }
    match policy {
        ReadOnlyWalPolicy::ExistingOnly => true,
        ReadOnlyWalPolicy::AllowDatabaseOwner => match (reader_uid, file_owner_uid(db_path)) {
            (Some(reader), Some(owner)) => reader != owner,
            _ => true,
        },
    }
}

/// Path of the WAL shared-memory sidecar of `db_path`: the full file name
/// with `-shm` appended (`pump.db` maps to `pump.db-shm`, not a re-extension).
fn wal_index_sidecar_path(db_path: &Path) -> PathBuf {
    let mut sidecar = db_path.as_os_str().to_os_string();
    sidecar.push("-shm");
    PathBuf::from(sidecar)
}

/// Whether the database header declares WAL journal mode: bytes 18 and 19
/// both hold 2 in the SQLite file format. Unreadable or short files are not
/// WAL databases.
fn database_header_is_wal(db_path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(db_path) else {
        return false;
    };
    let mut header = [0_u8; 20];
    file.read_exact(&mut header).is_ok() && header[18] == 2 && header[19] == 2
}

/// The uid that owns `db_path`, when the platform and the file both report
/// it. Unreadable or absent files report unknown ownership.
#[cfg(unix)]
fn file_owner_uid(db_path: &Path) -> Option<u32> {
    match fs::metadata(db_path) {
        Ok(meta) => Some(meta.uid()),
        Err(_) => None,
    }
}

/// See the Unix version: no ownership is provable on this platform.
#[cfg(not(unix))]
fn file_owner_uid(_db_path: &Path) -> Option<u32> {
    None
}

#[allow(clippy::cast_sign_loss)]
fn raw_stored_message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMessageView> {
    let remote_account: Option<String> = row.get(15)?;
    let remote = if let Some(account) = remote_account {
        Some(RemoteBindingInput {
            account,
            provider: row.get(16)?,
            remote_mailbox: row.get(17)?,
            remote_uid: row.get(18)?,
            remote_uidvalidity: row.get(19)?,
        })
    } else {
        None
    };
    let message_id: String = row.get(0)?;
    Ok(StoredMessageView {
        handle: message_id.clone(),
        message_id,
        account: row.get(1)?,
        content_id: row.get(2)?,
        blob_relpath: row.get(3)?,
        byte_size: row.get::<_, i64>(4)? as u64,
        local_role: row.get(5)?,
        read_state: row.get::<_, i64>(6)? != 0,
        starred: row.get::<_, i64>(7)? != 0,
        date: row.get(8)?,
        from_addr: row.get(9)?,
        to_addr: row.get(10)?,
        cc_addr: row.get(11)?,
        bcc_addr: row.get(12)?,
        subject: row.get(13)?,
        normalized_message_id: row.get(14)?,
        remote,
        absorbed_at: row.get(20)?,
        absorbed_by: row.get(21)?,
    })
}

fn blob_relpath(content_id: &str) -> String {
    format!(
        "{}/{}/{}/{}.eml",
        BLOBS_DIR,
        &content_id[..2],
        &content_id[2..4],
        content_id
    )
}

fn write_blob_if_absent(path: &Path, data: &[u8]) -> Result<bool, VivariumError> {
    if path.exists() {
        return Ok(false);
    }
    let Some(parent) = path.parent() else {
        return Err(VivariumError::Other(format!(
            "blob path has no parent: {}",
            path.display()
        )));
    };
    secure_create_dir_all(parent)
        .map_err(|e| VivariumError::Other(format!("failed to create blob dir: {e}")))?;
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(data)?;
            file.sync_all()?;
            #[cfg(unix)]
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(VivariumError::Io(e)),
    }
}

fn remote_bound_message_id(
    account: &str,
    local_role: &str,
    content_id: &str,
    remote: &RemoteBindingInput,
) -> String {
    let seed = format!(
        "remote\0{account}\0{local_role}\0{content_id}\0{}\0{}\0{}",
        remote.remote_mailbox, remote.remote_uidvalidity, remote.remote_uid
    );
    opaque_message_id(&seed)
}

fn fallback_message_id(request: &MessageIngestRequest, content_id: &str) -> String {
    let seed = format!(
        "local\0{}\0{}\0{}\0{}",
        request.account, request.local_role, request.seed_hint, content_id
    );
    opaque_message_id(&seed)
}

fn opaque_message_id(seed: &str) -> String {
    let hash = sha256_hex(seed.as_bytes());
    format!("msg_{}", &hash[..TRAVELING_ID_BASIS_LEN])
}

const TRAVELING_ID_PREFIX: &str = "msg_";
const TRAVELING_ID_BASIS_LEN: usize = 24;

/// Whether `id` has the traveling-id shape: `msg_` plus exactly 24 lowercase
/// hex characters (the 96-bit basis [`opaque_message_id`] emits).
///
/// This is the only shape ingest accepts as a presented id. It is strict on
/// purpose: ids arrive from other stores, and a lenient parse would let two
/// spellings of one id file as two items.
#[must_use]
pub fn is_traveling_id(id: &str) -> bool {
    id.strip_prefix(TRAVELING_ID_PREFIX).is_some_and(|basis| {
        basis.len() == TRAVELING_ID_BASIS_LEN
            && basis
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Short-handle width in characters of the message id's basis.
///
/// The basis is 24 hex characters (`opaque_message_id` keeps 12 bytes of a
/// SHA-256 digest), so this is 32 bits of that digest. The expected number of
/// colliding pairs across a 74k-message mailbox is 0.6; see
/// [`short_handle_map`] for what a collision costs.
pub(crate) const SHORT_HANDLE_LEN: usize = 8;

/// Short display handle of one message id.
///
/// A handle is a fixed-width prefix of the id's basis (the id without its
/// `msg_` prefix). Ids without the prefix are their own handle.
pub(crate) fn short_handle(message_id: &str) -> String {
    match message_id.strip_prefix("msg_") {
        None => message_id.to_string(),
        Some(basis) => {
            let len = usize::min(SHORT_HANDLE_LEN, basis.len());
            basis[..len].to_string()
        }
    }
}

/// Short display handles for the given message ids.
///
/// A handle is a fixed-width prefix of the id's basis (the id without its
/// `msg_` prefix), so it is a pure function of one id and independent of every
/// other id in the mailbox. Ids without the `msg_` prefix are their own handle.
///
/// An earlier version widened a handle until its prefix was unique across the
/// mailbox. Proving that uniqueness inserted every prefix of every id at every
/// length from 8 to the basis length: 17 prefixes per id, 1.26 million String
/// inserts and about 19 MB of transient allocation, measured at 1.05 seconds
/// on a 74k-message mailbox and paid by every command that materialized
/// handles. The widening had never once changed an answer — at this width the
/// live mailbox has no prefix collision at all. Two ids that share a prefix now
/// share a handle, and [`Storage::resolve_message_token`] reports the resulting
/// token as ambiguous rather than guessing; the honest remedy is to address
/// that message by its full id.
pub(crate) fn short_handle_map(message_ids: &[String]) -> HashMap<String, String> {
    message_ids
        .iter()
        .map(|message_id| (message_id.clone(), short_handle(message_id)))
        .collect()
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    let hash = Sha256::digest(data);
    hex::encode(hash)
}
