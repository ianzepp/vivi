//! The reply envelope: Markdown with TOML front matter between `+++` fences.
//!
//! Every role reply opens with the envelope. The front matter carries the
//! typed `answer` and its fields; the prose report is the body.

pub mod correct;
pub mod review;
pub mod roles;

use std::fmt;

use serde::de::DeserializeOwned;

const FENCE: &str = "+++";

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
pub enum EnvelopeError {
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
}

impl fmt::Display for EnvelopeError {
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
        }
    }
}

impl std::error::Error for EnvelopeError {}

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

fn split_pieces(text: &str) -> Result<Pieces<'_>, EnvelopeError> {
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
        return Err(EnvelopeError::NoOpening);
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
    Err(EnvelopeError::Unclosed)
}

/// Split a reply into its front matter and body. The first non-blank line
/// must be `+++`; the next `+++` line closes the front matter.
pub fn split(text: &str) -> Result<(&str, &str), EnvelopeError> {
    split_pieces(text).map(|pieces| (pieces.front, pieces.body))
}

/// Tables that belong in the front matter; written below the fence they are lost.
const FRONT_TABLES: [&str; 2] = ["[[follow_up]]", "[[check]]"];

/// The first body line that starts one of the front-matter tables.
fn table_below_fence(pieces: &Pieces<'_>) -> Option<EnvelopeError> {
    pieces.body.lines().enumerate().find_map(|(index, line)| {
        let trimmed = line.trim();
        FRONT_TABLES
            .iter()
            .find(|table| trimmed.starts_with(**table))
            .map(|table| EnvelopeError::TableBelowFence {
                line: pieces.body_line + index,
                table: (*table).to_string(),
            })
    })
}

/// Parse a reply into a typed envelope and run its conditional rules.
pub fn parse<T: DeserializeOwned + Validate>(text: &str) -> Result<Envelope<T>, EnvelopeError> {
    let pieces = split_pieces(text)?;
    if let Some(err) = table_below_fence(&pieces) {
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
        EnvelopeError::Toml {
            line: pieces.front_line + before,
            message: err.message().to_string(),
        }
    })?;
    front.validate().map_err(EnvelopeError::Invalid)?;
    Ok(Envelope {
        front,
        body: pieces.body.trim().to_string(),
    })
}

#[cfg(test)]
#[path = "envelope_test.rs"]
mod tests;
