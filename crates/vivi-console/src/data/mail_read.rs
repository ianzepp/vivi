//! Reading letters from a store opened read-only: a task's text and the
//! replies linked under it. The letters are blob files under `.vivi/`; the
//! index rows only say where.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use vivi_error::VivariumError;

use super::letter::{self, Letter, read_blob};
use super::needs::epoch_of;
use super::store::{db_err, mailspace_dir};
use crate::queries::{MESSAGE_BLOB, THREAD_CHILDREN};

/// Most replies read for one thread.
pub const THREAD_LIMIT: usize = 200;

/// An index row for a letter, before its file is read.
struct Located {
    content_id: String,
    relpath: String,
    date: Option<String>,
    from: Option<String>,
    subject: Option<String>,
}

/// Read the letter an index row names. The index's date, sender and subject
/// win; the letter's own headers fill what the index lacks.
fn load(root: &Path, found: &Located) -> Result<Letter, VivariumError> {
    let blob = read_blob(&mailspace_dir(root), &found.relpath)?;
    let mut letter = letter::parse(&blob.bytes);
    letter.at = found.date.as_deref().and_then(epoch_of).or(letter.at);
    for (own, indexed) in [
        (&mut letter.from, &found.from),
        (&mut letter.subject, &found.subject),
    ] {
        if let Some(text) = indexed.as_ref().filter(|text| !text.is_empty()) {
            own.clone_from(text);
        }
    }
    Ok(letter)
}

/// The letter of message `message_id`, front matter included, and its
/// content id (the key replies link to). `None` when the store has no such
/// message.
///
/// # Errors
/// Returns an error when the index or the letter file cannot be read.
pub fn task_letter(
    conn: &Connection,
    root: &Path,
    message_id: &str,
) -> Result<Option<(String, Letter)>, VivariumError> {
    let context = "look up a task letter";
    let found = conn
        .prepare_cached(MESSAGE_BLOB.sql)
        .and_then(|mut stmt| {
            stmt.query_row([message_id], |row| {
                Ok(Located {
                    relpath: row.get(0)?,
                    content_id: row.get(1)?,
                    date: row.get(2)?,
                    from: row.get(3)?,
                    subject: row.get(4)?,
                })
            })
            .optional()
        })
        .map_err(db_err(context))?;
    let Some(found) = found else {
        return Ok(None);
    };
    let letter = load(root, &found)?;
    Ok(Some((found.content_id, letter)))
}

fn children(conn: &Connection, parent: &str, limit: usize) -> Result<Vec<Located>, VivariumError> {
    let context = "list a letter's replies";
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut stmt = conn
        .prepare_cached(THREAD_CHILDREN.sql)
        .map_err(db_err(context))?;
    let rows = stmt
        .query_map(rusqlite::params![parent, limit], |row| {
            Ok(Located {
                content_id: row.get(0)?,
                relpath: row.get(1)?,
                date: row.get(2)?,
                from: row.get(3)?,
                subject: row.get(4)?,
            })
        })
        .map_err(db_err(context))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_err(context))
}

/// Every reply under the letter with content id `root_content`, oldest first
/// (insertion order within one second), at most [`THREAD_LIMIT`]. Lifecycle
/// event letters are left out and each reply's front matter is dropped.
///
/// # Errors
/// Returns an error when the index or a letter file cannot be read.
pub fn replies(
    conn: &Connection,
    root: &Path,
    root_content: &str,
) -> Result<Vec<Letter>, VivariumError> {
    let mut seen: HashSet<String> = HashSet::from([root_content.to_string()]);
    let mut frontier = vec![root_content.to_string()];
    let mut found: Vec<Letter> = Vec::new();
    while !frontier.is_empty() && found.len() < THREAD_LIMIT {
        let mut next = Vec::new();
        for parent in &frontier {
            for child in children(conn, parent, THREAD_LIMIT)? {
                if !seen.insert(child.content_id.clone()) {
                    continue;
                }
                let mut reply = load(root, &child)?;
                if !reply.is_event() {
                    reply.body = letter::strip_front(&reply.body).to_string();
                    found.push(reply);
                }
                next.push(child.content_id);
            }
        }
        frontier = next;
    }
    found.truncate(THREAD_LIMIT);
    letter::oldest_first(&mut found, |reply| reply.at);
    Ok(found)
}

#[cfg(test)]
#[path = "mail_read_test.rs"]
mod tests;
