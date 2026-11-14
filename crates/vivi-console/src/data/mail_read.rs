//! Reading letters from a store opened read-only: a task's text and the
//! replies linked under it. The letters are blob files under `.vivi/`; the
//! index rows only say where.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path};

use rusqlite::{Connection, OptionalExtension};
use vivi_error::VivariumError;

use super::store::{db_err, mailspace_dir};
use crate::queries::{MESSAGE_BLOB, THREAD_CHILDREN};

/// Most bytes read from one letter file.
const BLOB_MAX: u64 = 4 * 1024 * 1024;
/// Most replies read for one thread.
pub const THREAD_LIMIT: usize = 200;

/// One letter: who sent it, when, and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Letter {
    /// Epoch seconds from the `Date` header; `None` when it did not parse.
    pub at: Option<i64>,
    pub from: String,
    pub subject: String,
    pub text: String,
}

/// An index row for a letter, before its file is read.
struct Located {
    content_id: String,
    relpath: String,
    date: Option<String>,
    from: Option<String>,
    subject: Option<String>,
}

fn read_blob(root: &Path, relpath: &str) -> Result<Vec<u8>, VivariumError> {
    let relative = Path::new(relpath);
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(VivariumError::Other(format!(
            "the store names a letter file outside itself: {relpath}"
        )));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(mailspace_dir(root).join(relative))?
        .take(BLOB_MAX)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The text of a letter with LF line ends, and whether it is a lifecycle
/// event letter (`X-Vivi-Event`), which records a state change and not
/// thread content.
fn parse(bytes: &[u8]) -> (String, bool) {
    let Some(parsed) = mail_parser::MessageParser::default().parse(bytes) else {
        return (String::from_utf8_lossy(bytes).replace("\r\n", "\n"), false);
    };
    let text = parsed
        .body_text(0)
        .map(|body| body.replace("\r\n", "\n"))
        .unwrap_or_default();
    (text, parsed.header_raw("X-Vivi-Event").is_some())
}

/// `text` without a leading `+++` front matter fence.
#[must_use]
pub fn strip_front(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("+++\n") else {
        return text;
    };
    match rest.find("\n+++\n") {
        Some(end) => rest[end + "\n+++\n".len()..].trim_start_matches('\n'),
        None => text,
    }
}

fn epoch_of(date: Option<&str>) -> Option<i64> {
    let date = date?.trim();
    chrono::DateTime::parse_from_rfc2822(date)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(date))
        .map_or_else(|_| None, |when| Some(when.timestamp()))
}

fn letter(root: &Path, found: &Located) -> Result<(Letter, bool), VivariumError> {
    let bytes = read_blob(root, &found.relpath)?;
    let (text, event) = parse(&bytes);
    let letter = Letter {
        at: epoch_of(found.date.as_deref()),
        from: found.from.clone().unwrap_or_default(),
        subject: found.subject.clone().unwrap_or_default(),
        text,
    };
    Ok((letter, event))
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
    let (letter, _) = letter(root, &found)?;
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
                let (mut reply, event) = letter(root, &child)?;
                if !event {
                    reply.text = strip_front(&reply.text).to_string();
                    found.push(reply);
                }
                next.push(child.content_id);
            }
        }
        frontier = next;
    }
    found.truncate(THREAD_LIMIT);
    // Stable: replies from one second keep the order they were stored in.
    found.sort_by_key(|reply| reply.at.unwrap_or(0));
    Ok(found)
}

#[cfg(test)]
#[path = "mail_read_test.rs"]
mod tests;
