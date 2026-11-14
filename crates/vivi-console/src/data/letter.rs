//! The one reader of a stored letter: the blob file read read-only, its
//! headers and plain-text body decoded by `mail-parser`, and the `+++` front
//! matter of a need split off.
//!
//! Every page that shows a letter (the mail thread, a job's brief and
//! replies, a need and its thread) goes through here. A letter that does not
//! parse reads as its raw text, never as markup.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use vivi_core::pump::envelope::{self, Validate};
use vivi_error::VivariumError;

/// Most bytes read from one letter file. The mail page is the strictest
/// caller (it shows the whole body); the job and need pages show less of a
/// letter than this, so one cap fits every page.
pub const BLOB_MAX: u64 = 128 * 1024;

/// The bytes read from a letter file, and whether more was left unread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    pub bytes: Vec<u8>,
    pub cut: bool,
}

/// The file a blob path names under the mailspace directory `dir`, or `None`
/// for a path that would leave it (absolute, or with `..`).
fn blob_path(dir: &Path, relpath: &str) -> Option<PathBuf> {
    let relative = Path::new(relpath);
    relative
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
        .then(|| dir.join(relative))
}

/// Up to [`BLOB_MAX`] bytes of the letter file `relpath` under the mailspace
/// directory `dir`, opened read-only.
///
/// # Errors
/// Returns an error when the path leaves the mailspace or the file cannot be
/// read.
pub fn read_blob(dir: &Path, relpath: &str) -> Result<Blob, VivariumError> {
    let path = blob_path(dir, relpath).ok_or_else(|| {
        VivariumError::Other(format!(
            "the store names a letter file outside itself: {relpath}"
        ))
    })?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(BLOB_MAX + 1)
        .read_to_end(&mut bytes)?;
    let cut = bytes.len() as u64 > BLOB_MAX;
    bytes.truncate(usize::try_from(BLOB_MAX).unwrap_or(usize::MAX));
    Ok(Blob { bytes, cut })
}

/// What the pages show of one letter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Letter {
    /// The `X-Vivi-Kind` header, lowercased.
    pub kind: Option<String>,
    /// The `X-Vivi-Event` header, lowercased.
    pub event: Option<String>,
    /// Epoch seconds from the `Date` header; `None` when absent or bad.
    pub at: Option<i64>,
    pub from: String,
    pub subject: String,
    /// The first `text/plain` part with LF line ends. Not markdown-rendered.
    pub body: String,
}

impl Letter {
    /// True for a lifecycle event letter (`X-Vivi-Event` set and not empty):
    /// a record of a state change, not thread content.
    #[must_use]
    pub fn is_event(&self) -> bool {
        self.event.is_some()
    }
}

/// A header value on one line: folds joined by single spaces.
fn one_line(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse the raw bytes of a letter. Bytes that are not a message come back
/// as a letter whose body is the bytes as text.
#[must_use]
pub fn parse(raw: &[u8]) -> Letter {
    let Some(parsed) = mail_parser::MessageParser::default().parse(raw) else {
        return Letter {
            body: String::from_utf8_lossy(raw).replace("\r\n", "\n"),
            ..Letter::default()
        };
    };
    let header = |name: &str| {
        parsed
            .header_raw(name)
            .map(one_line)
            .filter(|value| !value.is_empty())
    };
    Letter {
        kind: header("X-Vivi-Kind").map(|value| value.to_ascii_lowercase()),
        event: header("X-Vivi-Event").map(|value| value.to_ascii_lowercase()),
        at: parsed.date().map(mail_parser::DateTime::to_timestamp),
        from: header("From").unwrap_or_default(),
        subject: parsed.subject().unwrap_or_default().to_string(),
        body: parsed
            .body_text(0)
            .map(|body| body.replace("\r\n", "\n"))
            .unwrap_or_default(),
    }
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

/// Order `items` oldest first by the epoch `key` gives. Items with no date
/// go last, and items of one second keep the order they came in.
pub fn oldest_first<T>(items: &mut [T], key: impl Fn(&T) -> Option<i64>) {
    items.sort_by_key(|item| key(item).unwrap_or(i64::MAX));
}

/// What a need letter says in its front matter that the console shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct NeedFacts {
    pub done_when: Vec<String>,
    #[serde(default)]
    pub target: Option<String>,
}

impl Validate for NeedFacts {
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

/// A need's text split into what it asks and its prose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NeedText {
    pub facts: NeedFacts,
    /// The prose below the front matter, or the whole body when there is none.
    pub prose: String,
    /// Set when the front matter is there but could not be read: why.
    pub front_error: Option<String>,
}

/// Split a need's body into front matter and prose. A body with no front
/// matter (an old-format need) is all prose with no `done_when`.
#[must_use]
pub fn need_text(body: &str) -> NeedText {
    if envelope::split(body).is_err() {
        return NeedText {
            prose: body.trim().to_string(),
            ..NeedText::default()
        };
    }
    match envelope::parse::<NeedFacts>(body) {
        Ok(parsed) => NeedText {
            facts: parsed.front,
            prose: parsed.body,
            front_error: None,
        },
        Err(error) => NeedText {
            prose: envelope::split(body)
                .map_or_else(|_| body.to_string(), |(_, prose)| prose.trim().to_string()),
            front_error: Some(error.to_string()),
            ..NeedText::default()
        },
    }
}

#[cfg(test)]
#[path = "letter_test.rs"]
mod tests;
