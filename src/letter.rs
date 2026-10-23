//! The letter format: Markdown with TOML front matter between `+++` fences.
//!
//! One letter format serves every stored letter: the TOML front matter carries
//! the typed fields and the Markdown prose is the body. A role reply envelope
//! is one kind of letter.

pub mod kinds;
pub mod message;
pub mod plain;
pub mod read;
pub mod records;
pub mod sugar;
pub mod work;

pub use kinds::{Front, Letter, LetterKind, parse_kind};
pub use message::of_message;

use std::fmt;

use serde::de::DeserializeOwned;

pub const FENCE: &str = "+++";

/// A parsed envelope: the typed front matter and the prose body.
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope<T> {
    pub front: T,
    pub body: String,
}

/// Conditional rules the TOML types cannot express.
pub trait Validate {
    fn validate(&self) -> Result<(), String>;
}

/// Why an envelope did not parse. `Display` is one line naming what to fix,
/// written to be pasted into a correction prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LetterError {
    NoOpening,
    Unclosed,
    Toml {
        line: usize,
        message: String,
    },
    /// A `[[follow_up]]` or `[[check]]` table written below the closing fence,
    /// where it would be read as prose and lost. `line` is 1-based.
    TableBelowFence {
        line: usize,
        table: String,
    },
    Invalid(String),
    /// A stored letter with no front matter: history, never executed.
    Legacy {
        handle: String,
        kind: String,
    },
    UnknownKind(String),
    UnknownFormat(String),
}

impl fmt::Display for LetterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoOpening => write!(
                f,
                "the reply must open with a line containing only `{FENCE}` (the front matter fence)"
            ),
            Self::Unclosed => write!(
                f,
                "the front matter is not closed: add a second line containing only `{FENCE}`"
            ),
            Self::Toml { line, message } => {
                write!(f, "front matter line {line}: {}", one_line(message))
            }
            Self::TableBelowFence { line, table } => write!(
                f,
                "line {line}: `{table}` is below the closing `{FENCE}` fence, where it is prose and is lost: move it inside the front matter"
            ),
            Self::Invalid(reason) => write!(f, "{}", one_line(reason)),
            Self::Legacy { handle, kind } => write!(
                f,
                "letter {handle} is in the old format (no front matter); it is history and cannot be executed: send a new {kind} with front matter"
            ),
            Self::UnknownKind(kind) if kind.is_empty() => write!(
                f,
                "letter has no `X-Vivi-Kind` header: a format-1 letter names its kind (need, want, task, memo, rule, pump, mail or chat)"
            ),
            Self::UnknownKind(kind) => write!(
                f,
                "unknown letter kind `{kind}`: expected need, want, task, memo, rule, pump, mail or chat"
            ),
            Self::UnknownFormat(value) if value.is_empty() => write!(
                f,
                "letter has a blank `X-Vivi-Format` header: a format-1 letter carries `X-Vivi-Format: 1`"
            ),
            Self::UnknownFormat(value) => write!(
                f,
                "letter format `{value}` is not known to this vivi (it reads format {FORMAT_VERSION}): upgrade vivi"
            ),
        }
    }
}

impl std::error::Error for LetterError {}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The pieces of a split reply, with the 1-based line the front matter
/// starts on (the line after the opening fence).
struct Pieces<'a> {
    front: &'a str,
    body: &'a str,
    front_line: usize,
    body_line: usize,
}

fn split_pieces(text: &str) -> Result<Pieces<'_>, LetterError> {
    let mut lines = text.split_inclusive('\n');
    let mut offset = 0;
    let mut line_no = 0;
    let mut opened = false;
    for line in lines.by_ref() {
        offset += line.len();
        line_no += 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        opened = trimmed == FENCE;
        break;
    }
    if !opened {
        return Err(LetterError::NoOpening);
    }
    let front_start = offset;
    for line in lines {
        if line.trim() == FENCE {
            return Ok(Pieces {
                front: &text[front_start..offset],
                body: &text[offset + line.len()..],
                front_line: line_no + 1,
                body_line: line_no + 2 + text[front_start..offset].matches('\n').count(),
            });
        }
        offset += line.len();
    }
    Err(LetterError::Unclosed)
}

/// Split a reply into its front matter and body. The first non-blank line
/// must be `+++`; the next `+++` line closes the front matter.
pub fn split(text: &str) -> Result<(&str, &str), LetterError> {
    split_pieces(text).map(|pieces| (pieces.front, pieces.body))
}

/// The prose of a stored letter body: what follows the front matter when the
/// body opens with it, else the body. Surrounding blank space is trimmed, as
/// `parse_letter` trims it.
#[must_use]
pub fn prose_of(body: &str) -> &str {
    match split(body) {
        Ok((_, prose)) => prose.trim(),
        Err(_) => body,
    }
}

/// Tables that belong in the front matter; written below the fence they are lost.
const FRONT_TABLES: [&str; 2] = ["[[follow_up]]", "[[check]]"];

/// The first body line that starts one of the front-matter tables.
fn table_below_fence(pieces: &Pieces<'_>) -> Option<LetterError> {
    pieces.body.lines().enumerate().find_map(|(index, line)| {
        let trimmed = line.trim();
        FRONT_TABLES
            .iter()
            .find(|table| trimmed.starts_with(**table))
            .map(|table| LetterError::TableBelowFence {
                line: pieces.body_line + index,
                table: (*table).to_string(),
            })
    })
}

/// Parse a role reply into a typed envelope and run its conditional rules.
/// A `[[follow_up]]` or `[[check]]` table below the closing fence is refused.
///
/// # Errors
///
/// Returns the `LetterError` naming what to fix.
pub fn parse<T: DeserializeOwned + Validate>(text: &str) -> Result<Envelope<T>, LetterError> {
    parse_with(text, true)
}

/// Parse a stored letter into a typed envelope and run its conditional rules.
/// The body is prose: a table line in it is not refused.
///
/// # Errors
///
/// Returns the `LetterError` naming what to fix.
pub fn parse_letter<T: DeserializeOwned + Validate>(
    text: &str,
) -> Result<Envelope<T>, LetterError> {
    parse_with(text, false)
}

fn parse_with<T: DeserializeOwned + Validate>(
    text: &str,
    tables: bool,
) -> Result<Envelope<T>, LetterError> {
    let pieces = split_pieces(text)?;
    if tables && let Some(err) = table_below_fence(&pieces) {
        return Err(err);
    }
    let front: T = toml::from_str(pieces.front).map_err(|err| {
        let before = err.span().map_or(0, |span| {
            let end = span.start.min(pieces.front.len());
            pieces.front.as_bytes()[..end]
                .iter()
                .filter(|&&byte| byte == b'\n')
                .count()
        });
        LetterError::Toml {
            line: pieces.front_line + before,
            message: err.message().to_string(),
        }
    })?;
    front.validate().map_err(LetterError::Invalid)?;
    Ok(Envelope {
        front,
        body: pieces.body.trim().to_string(),
    })
}

/// The header naming the letter format version.
pub const FORMAT_HEADER: &str = "X-Vivi-Format";

/// The letter format version this vivi reads and writes.
pub const FORMAT_VERSION: &str = "1";

/// Render front matter and prose as a letter.
///
/// # Errors
///
/// Returns `LetterError::Invalid` when the front matter does not serialize or
/// holds a line that is only `+++`, which would close the front matter early.
pub fn render<T: serde::Serialize>(front: &T, prose: &str) -> Result<String, LetterError> {
    let toml = toml::to_string(front)
        .map_err(|err| LetterError::Invalid(format!("front matter does not render: {err}")))?;
    if toml.lines().any(|line| line.trim() == FENCE) {
        return Err(LetterError::Invalid(format!(
            "front matter value has a `{FENCE}` line, which would close the front matter"
        )));
    }
    Ok(format!("{FENCE}\n{toml}{FENCE}\n\n{prose}"))
}

/// Serde `skip_serializing_if` helper for a flag that is omitted when false.
#[allow(clippy::trivially_copy_pass_by_ref)]
#[must_use]
pub fn is_false(value: &bool) -> bool {
    !*value
}

/// A front matter text field must not be blank.
///
/// # Errors
///
/// Returns the message naming `key` when the trimmed value is empty.
pub fn check_text(key: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("`{key}` must not be blank"));
    }
    Ok(())
}

/// A front matter list must have no blank entry.
///
/// # Errors
///
/// Returns the message naming `key` and the 1-based position of the first blank entry.
pub fn check_list(key: &str, items: &[String]) -> Result<(), String> {
    match items.iter().position(|item| item.trim().is_empty()) {
        Some(index) => Err(format!("`{key}` entry {} is blank", index + 1)),
        None => Ok(()),
    }
}

#[cfg(test)]
#[path = "letter_test.rs"]
mod tests;

#[cfg(test)]
#[path = "letter/fixture_test.rs"]
pub(crate) mod fixture;
