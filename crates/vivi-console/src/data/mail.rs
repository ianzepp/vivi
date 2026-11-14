//! The mail page's data: thread roots filed under an identity and folder,
//! and one thread read in order.
//!
//! A *thread* is the set of letters joined by `mailspace_links` (a reply
//! names its parent by content id). The list shows each thread once, as its
//! root letter; the thread page reads the whole set, the way `vivi thread`
//! does, with the same links. Nothing here opens the mailspace for writing:
//! the store is read with [`ReadStore`] and bodies are read from the blob
//! files the store names.

use std::path::Path;

use rusqlite::Connection;
use vivi_error::VivariumError;
use vivi_storage::storage::short_handle;

use super::config;
use super::letter::{self, BLOB_MAX, read_blob};
use super::store::{ReadStore, db_err, mailspace_dir};
use crate::queries::{MAIL_RESOLVE, MAIL_THREAD, MAIL_THREADS};

/// Thread roots listed.
pub const LIST_ROWS: usize = 50;
/// Letters read for one thread.
pub const THREAD_LIMIT: i64 = 200;
/// The longest handle a request may carry.
const MAX_TOKEN: usize = 100;

/// The folders (`local_role`) the list scans, in display order.
pub const ROLES: [&str; 7] = ["inbox", "tasks", "needs", "wants", "memos", "done", "sent"];
/// The kinds the list can filter by. A kind is read from the folder a letter
/// is filed in, which is what an index can answer without opening the blob.
pub const KINDS: [&str; 6] = ["mail", "task", "need", "want", "memo", "done"];

/// The kind a folder stands for.
#[must_use]
pub fn kind_of_role(role: &str) -> &'static str {
    match role {
        "tasks" => "task",
        "needs" => "need",
        "wants" => "want",
        "memos" => "memo",
        "done" => "done",
        _ => "mail",
    }
}

fn roles_of_kind(kind: &str) -> &'static [&'static str] {
    match kind {
        "mail" => &["inbox", "sent"],
        "task" => &["tasks"],
        "need" => &["needs"],
        "want" => &["wants"],
        "memo" => &["memos"],
        "done" => &["done"],
        _ => &[],
    }
}

/// The query-string filters; an empty one keeps everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailFilter {
    pub identity: String,
    pub role: String,
    pub kind: String,
}

/// One thread, as its root letter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRow {
    pub handle: String,
    pub identity: String,
    pub role: String,
    pub subject: String,
    pub from: String,
    pub date: String,
    pub updated_at: String,
    /// Direct replies to the root.
    pub replies: i64,
    content_id: String,
}

impl ThreadRow {
    /// The kind of the folder the root is filed in.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        kind_of_role(&self.role)
    }
}

/// The thread list and the names the filter chips offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailList {
    /// The project's role names and their old names.
    pub identities: Vec<String>,
    pub rows: Vec<ThreadRow>,
}

/// One letter of a thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadMessage {
    pub handle: String,
    /// The handle of the letter this one answers, when it is in the thread.
    pub parent_handle: Option<String>,
    pub date: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub kind: Option<String>,
    pub event: Option<String>,
    pub body: String,
    /// Set when the body was cut at [`BLOB_MAX`], or the file is missing.
    pub note: Option<String>,
}

/// A thread, oldest letter first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub messages: Vec<ThreadMessage>,
}

/// What looking up a handle found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadState {
    NoStore,
    NotFound,
    /// The handle matches more than one letter: the ids it matches.
    Ambiguous(Vec<String>),
    Ready(Thread),
}

/// The names a role may be filed under: each identity and its old names.
fn identity_names(config: &vivi_core::mailspace::MailspaceConfig) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for identity in &config.identities {
        for name in std::iter::once(&identity.name).chain(&identity.aliases) {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
    }
    names.sort();
    names
}

/// List thread roots for `filter`; `None` when the project has no store.
///
/// # Errors
/// Returns an error when the config or the store cannot be read.
pub fn list(root: &Path, filter: &MailFilter) -> Result<Option<MailList>, VivariumError> {
    let identities = identity_names(&config::load(root)?);
    let Some(store) = ReadStore::open(root)? else {
        return Ok(None);
    };
    let accounts: Vec<String> = if filter.identity.is_empty() {
        identities.clone()
    } else {
        vec![filter.identity.clone()]
    };
    let roles = roles_for(filter);
    let rows = store.read(|conn| {
        let mut rows = Vec::new();
        for account in &accounts {
            for role in &roles {
                rows.extend(roots(conn, account, role)?);
            }
        }
        Ok(rows)
    })?;
    Ok(Some(MailList {
        identities,
        rows: newest_distinct(rows),
    }))
}

/// The folders the filter allows.
fn roles_for(filter: &MailFilter) -> Vec<&'static str> {
    let by_kind = (!filter.kind.is_empty()).then(|| roles_of_kind(&filter.kind));
    ROLES
        .iter()
        .copied()
        .filter(|role| filter.role.is_empty() || filter.role == *role)
        .filter(|role| by_kind.is_none_or(|kinds| kinds.contains(role)))
        .collect()
}

/// Newest first, one row per letter (a letter is filed once per holder).
fn newest_distinct(mut rows: Vec<ThreadRow>) -> Vec<ThreadRow> {
    rows.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.handle.cmp(&b.handle))
    });
    let mut seen = std::collections::HashSet::new();
    rows.retain(|row| seen.insert(row.content_id.clone()));
    rows.truncate(LIST_ROWS);
    rows
}

fn roots(conn: &Connection, account: &str, role: &str) -> Result<Vec<ThreadRow>, VivariumError> {
    let context = "list mail threads";
    let mut stmt = conn
        .prepare_cached(MAIL_THREADS.sql)
        .map_err(db_err(context))?;
    let limit = i64::try_from(LIST_ROWS).unwrap_or(50);
    let found = stmt
        .query_map(rusqlite::params![account, role, limit], |row| {
            let message_id: String = row.get(0)?;
            Ok(ThreadRow {
                handle: short_handle(&message_id),
                identity: row.get(1)?,
                role: row.get(2)?,
                content_id: row.get(3)?,
                updated_at: row.get(4)?,
                date: row.get(5)?,
                from: row.get(6)?,
                subject: row.get(7)?,
                replies: row.get(8)?,
            })
        })
        .map_err(db_err(context))?;
    found
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err(context))
}

/// Read the thread that holds the letter `token` names (a handle, or an id).
///
/// # Errors
/// Returns an error when the store cannot be read.
pub fn thread(root: &Path, token: &str) -> Result<ThreadState, VivariumError> {
    let token = token.trim();
    if token.is_empty() || token.len() > MAX_TOKEN {
        return Ok(ThreadState::NotFound);
    }
    let Some(store) = ReadStore::open(root)? else {
        return Ok(ThreadState::NoStore);
    };
    let dir = mailspace_dir(root);
    store.read(|conn| {
        let content = match resolve(conn, token)? {
            Resolved::Found(content) => content,
            Resolved::None => return Ok(ThreadState::NotFound),
            Resolved::Many(ids) => return Ok(ThreadState::Ambiguous(ids)),
        };
        Ok(ThreadState::Ready(read_thread(conn, &dir, &content)?))
    })
}

enum Resolved {
    /// The content id of the one letter the handle names.
    Found(String),
    None,
    Many(Vec<String>),
}

/// Resolve a handle the way `vivi` does: an exact id, else a prefix of the
/// id, else a prefix after the `msg_` marker. More than one match is
/// ambiguous, never a guess.
fn resolve(conn: &Connection, token: &str) -> Result<Resolved, VivariumError> {
    let context = "resolve a mail handle";
    let mut stmt = conn
        .prepare_cached(MAIL_RESOLVE.sql)
        .map_err(db_err(context))?;
    let mut found: Vec<(String, String)> = Vec::new();
    for prefix in [token.to_string(), format!("msg_{token}")] {
        let upper = format!("{prefix}{}", char::MAX);
        let rows = stmt
            .query_map(rusqlite::params![prefix, upper], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(db_err(context))?;
        for row in rows {
            let row = row.map_err(db_err(context))?;
            if !found.contains(&row) {
                found.push(row);
            }
        }
    }
    let exact = found
        .iter()
        .find(|(id, _)| id == token || *id == format!("msg_{token}"));
    Ok(match (exact, found.len()) {
        (Some((_, content)), _) => Resolved::Found(content.clone()),
        (None, 0) => Resolved::None,
        (None, 1) => Resolved::Found(found[0].1.clone()),
        (None, _) => Resolved::Many(found.into_iter().map(|(id, _)| id).collect()),
    })
}

struct Raw {
    content_id: String,
    date: String,
    from: String,
    to: String,
    cc: String,
    subject: String,
    relpath: String,
    message_id: Option<String>,
    stored_at: Option<String>,
    parent: Option<String>,
}

fn read_thread(conn: &Connection, dir: &Path, content: &str) -> Result<Thread, VivariumError> {
    let context = "read a mail thread";
    let mut stmt = conn
        .prepare_cached(MAIL_THREAD.sql)
        .map_err(db_err(context))?;
    let found = stmt
        .query_map(rusqlite::params![content, THREAD_LIMIT], |row| {
            Ok(Raw {
                content_id: row.get(0)?,
                date: row.get(1)?,
                from: row.get(2)?,
                to: row.get(3)?,
                cc: row.get(4)?,
                subject: row.get(5)?,
                relpath: row.get(6)?,
                message_id: row.get(7)?,
                stored_at: row.get(8)?,
                parent: row.get(9)?,
            })
        })
        .map_err(db_err(context))?;
    let mut raws = found
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err(context))?;
    // `Date` has one-second precision: replies in the same second fall back
    // to when each was stored, as `vivi thread` orders them.
    raws.sort_by(|a, b| {
        (&a.date, &a.stored_at, &a.content_id).cmp(&(&b.date, &b.stored_at, &b.content_id))
    });
    let handles: Vec<(String, String)> = raws
        .iter()
        .map(|raw| (raw.content_id.clone(), handle_of(raw)))
        .collect();
    let messages = raws
        .iter()
        .map(|raw| message_of(dir, raw, &handles))
        .collect();
    Ok(Thread { messages })
}

fn handle_of(raw: &Raw) -> String {
    match &raw.message_id {
        Some(id) => short_handle(id),
        None => raw.content_id.chars().take(8).collect(),
    }
}

fn message_of(dir: &Path, raw: &Raw, handles: &[(String, String)]) -> ThreadMessage {
    let (letter, note) = match read_blob(dir, &raw.relpath) {
        Ok(blob) => (
            letter::parse(&blob.bytes),
            blob.cut
                .then(|| format!("Only the first {} KiB is shown.", BLOB_MAX / 1024)),
        ),
        Err(_) => (
            letter::parse(b""),
            Some("The letter's file could not be read.".to_string()),
        ),
    };
    let parent_handle = raw.parent.as_ref().and_then(|parent| {
        handles
            .iter()
            .find(|(content, _)| content == parent)
            .map(|(_, handle)| handle.clone())
    });
    ThreadMessage {
        handle: handle_of(raw),
        parent_handle,
        date: raw.date.clone(),
        from: raw.from.clone(),
        to: raw.to.clone(),
        cc: raw.cc.clone(),
        subject: raw.subject.clone(),
        kind: letter.kind,
        event: letter.event,
        body: letter.body,
        note,
    }
}

#[cfg(test)]
#[path = "mail_test.rs"]
pub(crate) mod tests;
