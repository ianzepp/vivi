//! The need page's data: the backlog node, the letter and its thread, the
//! attempts, and the flow tab's pump view.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, ToSql};
use vivi_error::VivariumError;

use super::letter::{self, NeedText, read_blob};
use super::need_flow::{self, FlowView, PumpState};
use super::needs::{AttemptRow, Attempts, NeedNode, attempts_of, epoch_of, node_from_row};
use super::pump_db::PumpRead;
use super::store::{ReadStore, db_err, has_table, mailspace_dir};
use crate::queries::{LETTER_OF_CONTENT, NEED_MESSAGE, NEED_NODE, THREAD_OF_CONTENT};

/// Attempts listed on the attempts tab.
pub const ATTEMPT_LIMIT: i64 = 100;
/// Replies read from a need's thread.
pub const THREAD_LIMIT: i64 = 60;

/// Which tab of the need page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Attempts,
    Flow,
}

impl Tab {
    /// The tab a query-string word names; anything else is `Attempts`.
    #[must_use]
    pub fn parse(word: &str) -> Self {
        if word == "flow" {
            Self::Flow
        } else {
            Self::Attempts
        }
    }

    /// The word the query string uses.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Attempts => "attempts",
            Self::Flow => "flow",
        }
    }
}

/// The need's own letter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedLetter {
    pub text: NeedText,
    pub from: String,
    pub date: String,
}

/// One reply in the need's thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub date: String,
    pub epoch: Option<i64>,
    pub from: String,
    pub subject: String,
    pub body: String,
}

/// The attempts tab's extra data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// `None` when no stored copy of the need letter was found.
    pub letter: Option<NeedLetter>,
    pub replies: Vec<Reply>,
    /// Lifecycle records (done, marked, ...) left out of the replies.
    pub records: usize,
}

/// What the flow tab found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowTab {
    /// The need has no flow run in the pump store.
    NoRuns,
    Runs(Box<FlowView>),
}

/// Everything the need page shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedPage {
    pub handle: String,
    pub node: Option<NeedNode>,
    /// `None` when the store has no slot index.
    pub attempts: Option<Attempts>,
    pub thread: Option<Thread>,
    pub flow: Option<PumpState<FlowTab>>,
}

impl NeedPage {
    /// True when something records this handle as a need.
    #[must_use]
    pub fn exists(&self) -> bool {
        let ran = self.attempts.as_ref().is_some_and(|a| a.count > 0);
        let in_flow = matches!(&self.flow, Some(PumpState::Ready(FlowTab::Runs(_))));
        self.node.is_some() || ran || in_flow
    }

    /// The attempts, newest first.
    #[must_use]
    pub fn attempt_rows(&self) -> &[AttemptRow] {
        self.attempts.as_ref().map_or(&[], |a| a.newest.as_slice())
    }
}

/// Load the page for `handle`; `None` when the project has no mailspace store.
///
/// # Errors
/// Returns an error when the mailspace store cannot be read.
pub fn load(
    root: &Path,
    handle: &str,
    tab: Tab,
    run: Option<i64>,
) -> Result<Option<NeedPage>, VivariumError> {
    let Some(store) = ReadStore::open(root)? else {
        return Ok(None);
    };
    let pump = (tab == Tab::Flow).then(|| need_flow::open_pump(root));
    let mut page = store.read(|conn| {
        let node = need_node(conn, handle)?;
        let attempts = attempts_of(conn, handle, ATTEMPT_LIMIT)?;
        let thread = match tab {
            Tab::Attempts => Some(thread_of(conn, root, handle)?),
            Tab::Flow => None,
        };
        Ok(NeedPage {
            handle: handle.to_string(),
            node,
            attempts,
            thread,
            flow: None,
        })
    })?;
    page.flow = pump.map(|state| flow_tab(state, handle, run));
    Ok(Some(page))
}

fn flow_tab(state: PumpState<PumpRead>, handle: &str, run: Option<i64>) -> PumpState<FlowTab> {
    let read = match state {
        PumpState::NoPump => return PumpState::NoPump,
        PumpState::NotStarted => return PumpState::NotStarted,
        PumpState::Unreadable(why) => return PumpState::Unreadable(why),
        PumpState::Ready(read) => read,
    };
    let loaded = read.read(|pconn| {
        if !need_flow::has_flow_tables(pconn)? {
            return Err(VivariumError::Other(String::from(
                "the pump store has no flow tables yet",
            )));
        }
        need_flow::load_flow(pconn, handle, run)
    });
    match loaded {
        Ok(view) if view.runs.is_empty() => PumpState::Ready(FlowTab::NoRuns),
        Ok(view) => PumpState::Ready(FlowTab::Runs(Box::new(view))),
        Err(error) => PumpState::Unreadable(error.to_string()),
    }
}

fn need_node(conn: &Connection, handle: &str) -> Result<Option<NeedNode>, VivariumError> {
    if !has_table(conn, "work_graph_nodes")? {
        return Ok(None);
    }
    conn.prepare_cached(NEED_NODE.sql)
        .and_then(|mut stmt| stmt.query_row([handle], node_from_row).optional())
        .map_err(db_err("read the need"))
}

/// The smallest string above every string that starts with `prefix`.
fn prefix_end(prefix: &str) -> Option<String> {
    let mut bytes = prefix.as_bytes().to_vec();
    let last = bytes.last_mut()?;
    if !last.is_ascii() || *last == 0x7f {
        return None;
    }
    *last += 1;
    match String::from_utf8(bytes) {
        Ok(end) => Some(end),
        Err(_) => None,
    }
}

/// The content id of a stored copy of the need whose handle is `handle`.
fn need_content(conn: &Connection, handle: &str) -> Result<Option<String>, VivariumError> {
    for prefix in [format!("msg_{handle}"), handle.to_string()] {
        let Some(end) = prefix_end(&prefix) else {
            continue;
        };
        let found = conn
            .prepare_cached(NEED_MESSAGE.sql)
            .and_then(|mut stmt| {
                stmt.query_row([&prefix, &end], |row| row.get::<_, String>(1))
                    .optional()
            })
            .map_err(db_err("find the need letter"))?;
        if found.is_some() {
            return Ok(found);
        }
    }
    Ok(None)
}

/// A letter's metadata and where its bytes live.
struct Stored {
    date: String,
    from: String,
    subject: String,
    relpath: String,
}

/// The bytes of a stored letter, or `None` when its file cannot be read.
fn read_letter_bytes(root: &Path, relpath: &str) -> Option<Vec<u8>> {
    match read_blob(&mailspace_dir(root), relpath) {
        Ok(blob) => Some(blob.bytes),
        Err(_) => None,
    }
}

fn thread_of(conn: &Connection, root: &Path, handle: &str) -> Result<Thread, VivariumError> {
    let Some(content) = need_content(conn, handle)? else {
        return Ok(Thread {
            letter: None,
            replies: Vec::new(),
            records: 0,
        });
    };
    let letter = need_letter(conn, root, &content)?;
    let context = "read the need's thread";
    let mut stmt = conn
        .prepare_cached(THREAD_OF_CONTENT.sql)
        .map_err(db_err(context))?;
    let params: [&dyn ToSql; 2] = [&content, &THREAD_LIMIT];
    let stored = stmt
        .query_map(params, |row| stored_from_row(row, 1))
        .map_err(db_err(context))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err(context))?;
    let mut replies = Vec::new();
    let mut records = 0;
    for item in stored {
        let Some(bytes) = read_letter_bytes(root, &item.relpath) else {
            continue;
        };
        let parsed = letter::parse(&bytes);
        if parsed.is_event() {
            records += 1;
            continue;
        }
        replies.push(reply_of(&item, parsed.body));
    }
    letter::oldest_first(&mut replies, |reply| reply.epoch);
    Ok(Thread {
        letter,
        replies,
        records,
    })
}

fn stored_from_row(row: &Row<'_>, first: usize) -> rusqlite::Result<Stored> {
    Ok(Stored {
        date: row.get(first)?,
        from: row.get(first + 1)?,
        subject: row.get(first + 2)?,
        relpath: row.get(first + 3)?,
    })
}

fn need_letter(
    conn: &Connection,
    root: &Path,
    content: &str,
) -> Result<Option<NeedLetter>, VivariumError> {
    let stored = conn
        .prepare_cached(LETTER_OF_CONTENT.sql)
        .and_then(|mut stmt| {
            stmt.query_row([content], |row| stored_from_row(row, 0))
                .optional()
        })
        .map_err(db_err("read the need letter"))?;
    let Some(stored) = stored else {
        return Ok(None);
    };
    let Some(bytes) = read_letter_bytes(root, &stored.relpath) else {
        return Ok(None);
    };
    Ok(Some(NeedLetter {
        text: letter::need_text(&letter::parse(&bytes).body),
        from: stored.from,
        date: stored.date,
    }))
}

fn reply_of(item: &Stored, body: String) -> Reply {
    Reply {
        date: item.date.clone(),
        epoch: epoch_of(&item.date),
        from: item.from.clone(),
        subject: item.subject.clone(),
        body,
    }
}
