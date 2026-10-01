use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::error::VivariumError;
use crate::store::secure_create_dir_all;

mod backlog_graph;
pub use backlog_graph::{
    BACKLOG_GRAPH_CODE, BacklogMintCommit, BacklogMintInput, BacklogNodeInput,
};
mod events;
mod goals;
mod graph;
mod handles;
mod ingest;
mod item_metadata;
mod links;
mod metadata;
mod mutate;
mod query;
mod schema;
#[cfg(test)]
mod tests;

pub use goals::{GoalRow, goal_handle_for_path};
pub use graph::{
    WorkGraphActivateInput, WorkGraphApplyPlan, WorkGraphAttemptRow, WorkGraphEdgeInput,
    WorkGraphEdgeRow, WorkGraphEventRow, WorkGraphImportCommit, WorkGraphImportInput,
    WorkGraphNodeInput, WorkGraphNodeRow, WorkGraphRow, edge_handle_for, graph_handle_for_code,
    node_handle_for,
};
pub use links::MailspaceLink;
use metadata::parse_metadata;
pub use mutate::MailspaceMoveWithReply;
use schema::{ensure_schema, message_query};

const MAILSPACE_DB_FILENAME: &str = "mail.sqlite";
const BLOBS_DIR: &str = "blobs";

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
    pub message_id_hint: Option<String>,
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

pub struct Storage {
    mail_root: PathBuf,
    conn: Connection,
    /// Cached handle index, cleared on any write.
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

    fn open_with_db(mail_root: &Path, db_path: &Path) -> Result<Self, VivariumError> {
        secure_create_dir_all(&mail_root.join(BLOBS_DIR))
            .map_err(|e| VivariumError::Other(format!("failed to create blob dir: {e}")))?;

        let conn = Connection::open(db_path)
            .map_err(|e| VivariumError::Other(format!("failed to open storage database: {e}")))?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|e| VivariumError::Other(format!("failed to set SQLite timeout: {e}")))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| VivariumError::Other(format!("failed to set WAL journal mode: {e}")))?;
        #[cfg(unix)]
        fs::set_permissions(db_path, fs::Permissions::from_mode(0o600))?;

        ensure_schema(&conn)?;

        Ok(Self {
            mail_root: mail_root.to_path_buf(),
            conn,
            handle_index: RefCell::new(None),
        })
    }

    /// Clear the cached handle index after any write that affects messages.
    fn invalidate_handle_index(&self) {
        *self.handle_index.borrow_mut() = None;
    }
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
    format!("msg_{}", &hash[..24])
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
fn short_handle(message_id: &str) -> String {
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
fn short_handle_map(message_ids: &[String]) -> HashMap<String, String> {
    message_ids
        .iter()
        .map(|message_id| (message_id.clone(), short_handle(message_id)))
        .collect()
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    let hash = Sha256::digest(data);
    hex::encode(hash)
}
