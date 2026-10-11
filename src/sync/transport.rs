//! The letter wire as the sync passes see it: one append, one read page.
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
    /// Standard base64 of the exact bytes appended.
    pub raw_eml: String,
}

/// One page of records after a cursor.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Page {
    #[serde(default)]
    pub records: Vec<Record>,
    pub next_cursor: u64,
}

/// A project server, as the passes use it.
pub trait Transport {
    /// Append one letter under its client-minted id.
    ///
    /// # Errors
    /// Returns [`AppendFailure`] saying whether the pass may continue.
    fn append(&self, message_id: &str, envelope: &Envelope<'_>) -> Result<Appended, AppendFailure>;

    /// Read the records after `since`, in sequence order.
    ///
    /// # Errors
    /// Returns an error when the server cannot be reached or refuses the
    /// read.
    fn fetch(&self, since: u64) -> Result<Page, VivariumError>;
}
