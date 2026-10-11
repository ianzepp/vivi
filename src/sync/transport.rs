//! The letter wire as the sync passes see it: one append, one read page, one
//! lookup by handle.
//!
//! Push and pull talk to a [`Transport`], so the passes run unchanged against
//! the real HTTP server and against an in-memory fake in tests.

use serde::Deserialize;

use crate::error::VivariumError;

/// A letter and what travels beside it on append: the declared recipient and
/// kind. The sender is never client input; the server takes it from the
/// bearer.
#[derive(Debug, Clone)]
pub struct Envelope<'a> {
    pub raw_eml: &'a [u8],
    pub to: &'a str,
    /// `need`, `task`, `want`, or `mail`.
    pub kind: &'a str,
}

/// The server's answer to an accepted append (200 or 201).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Appended {
    /// The id the server holds the letter under. A duplicate presented under
    /// a different id names the original.
    pub message_id: String,
    pub sequence: u64,
    pub duplicate: bool,
    /// The hash the server computed, when the response carries it.
    pub content_sha256: Option<String>,
}

/// Why an append did not land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendFailure {
    /// The server refused this one letter (400, 409, 413): the pass goes on.
    Refused(String),
    /// The pass cannot continue (429, 401, network failure, server error).
    Stop(String),
}

/// One record of a read page.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Record {
    pub sequence: u64,
    pub message_id: String,
    pub from: String,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    pub content_sha256: String,
    /// The authenticated sender's registered address prefix (8 lowercase hex
    /// characters), stamped by the server; absent for a legacy identity. A
    /// From address claiming another prefix is a forgery.
    #[serde(default)]
    pub from_prefix: Option<String>,
    /// When the server appended the letter (RFC 3339, server clock); absent
    /// for a row written before the server stamped letters.
    #[serde(default)]
    pub appended_at: Option<String>,
    /// Standard base64 of the exact bytes appended.
    pub raw_eml: String,
}

/// One page of records after a cursor.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Page {
    #[serde(default)]
    pub records: Vec<Record>,
    pub next_cursor: u64,
    /// Whether more records match after `next_cursor`. Absent from a server
    /// that does not page; the reader then falls back to "an empty page, or
    /// one that did not advance, ends the read".
    #[serde(default)]
    pub has_more: Option<bool>,
}

/// Records for a handle lookup, and the handles that matched nothing the
/// caller may see.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Lookup {
    #[serde(default)]
    pub records: Vec<Record>,
    #[serde(default)]
    pub missing: Vec<String>,
}

/// Records per list request the client asks for.
pub const PAGE_LIMIT: u32 = 500;

/// One list request: records after `since`, at most `limit`, optionally only
/// letters appended at or after `after` (RFC 3339).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListQuery<'a> {
    pub since: u64,
    pub limit: u32,
    pub after: Option<&'a str>,
}

impl ListQuery<'_> {
    /// A request for the records after `since`, with the standard page size.
    #[must_use]
    pub fn after_sequence(since: u64) -> ListQuery<'static> {
        ListQuery {
            since,
            limit: PAGE_LIMIT,
            after: None,
        }
    }
}

/// A project server, as the passes use it.
pub trait Transport {
    /// Append one letter under its client-minted id.
    ///
    /// # Errors
    /// Returns [`AppendFailure`] saying whether the pass may continue.
    fn append(&self, message_id: &str, envelope: &Envelope<'_>) -> Result<Appended, AppendFailure>;

    /// Read one page of records, in sequence order.
    ///
    /// # Errors
    /// Returns an error when the server cannot be reached or refuses the
    /// read.
    fn fetch(&self, query: &ListQuery<'_>) -> Result<Page, VivariumError>;

    /// Look up letters by handle (8 hex characters of a message id). A
    /// lookup never moves a cursor.
    ///
    /// # Errors
    /// Returns an error when the server cannot be reached or refuses the
    /// lookup.
    fn lookup(&self, _handles: &[String]) -> Result<Lookup, VivariumError> {
        Err(VivariumError::Message(
            "this project server connection cannot look up handles".into(),
        ))
    }
}
