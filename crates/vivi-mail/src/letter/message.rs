//! Reading a stored letter: the headers pick the kind, the body is the letter.

use crate::letter::kinds::{Letter, LetterKind, parse_kind};
use crate::letter::{FORMAT_HEADER, FORMAT_VERSION, LetterError};

const KIND_HEADER: &str = "X-Vivi-Kind";
const EVENT_HEADER: &str = "X-Vivi-Event";

/// The content headers W3-B4 moved into front matter. A format-1 letter
/// that carries one states a fact in a retired place, and the header is
/// never read, so the parse refuses it; the fact belongs in the front
/// matter. A legacy letter (no `X-Vivi-Format`) keeps them ignored as
/// history.
const MOVED_CONTENT_HEADERS: [&str; 7] = [
    "X-Vivi-Verdict",
    "X-Vivi-Repo",
    "X-Vivi-Branch",
    "X-Vivi-Commit",
    "X-Vivi-Tag",
    "X-Vivi-Rule-Scope",
    "X-Vivi-Supersedes",
];

/// The values of the headers a letter read needs, from the header block only.
#[derive(Default)]
struct Headers {
    format: Option<String>,
    kind: Option<String>,
    event: Option<String>,
    /// The first moved content header the header block carries, if any.
    moved: Option<&'static str>,
}

fn read_headers(raw: &[u8]) -> Headers {
    let mut headers = Headers::default();
    for line in raw.split(|byte| *byte == b'\n') {
        let line = String::from_utf8_lossy(line);
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if headers.moved.is_none()
            && let Some(moved) = MOVED_CONTENT_HEADERS
                .iter()
                .copied()
                .find(|moved| name.eq_ignore_ascii_case(moved))
        {
            headers.moved = Some(moved);
        }
        let slot = if name.eq_ignore_ascii_case(FORMAT_HEADER) {
            &mut headers.format
        } else if name.eq_ignore_ascii_case(KIND_HEADER) {
            &mut headers.kind
        } else if name.eq_ignore_ascii_case(EVENT_HEADER) {
            &mut headers.event
        } else {
            continue;
        };
        if slot.is_none() {
            *slot = Some(value.trim().to_string());
        }
    }
    headers
}

/// The MIME-decoded text body of a stored letter, with LF line endings.
fn body_text(raw: &[u8], handle: &str) -> Result<String, LetterError> {
    let message = mail_parser::MessageParser::default()
        .parse(raw)
        .ok_or_else(|| LetterError::Invalid(format!("letter {handle} is not a mail message")))?;
    Ok(message
        .body_text(0)
        .map(|text| text.replace("\r\n", "\n"))
        .unwrap_or_default())
}

/// Read a stored letter into a typed `Letter`. `handle` is the display handle
/// the caller shows; raw bytes do not carry it.
///
/// # Errors
///
/// Returns `LetterError::Legacy` for a letter with no `X-Vivi-Format` header
/// (history, never parsed), `LetterError::UnknownFormat` for a format this
/// vivi does not read, `LetterError::Invalid` for a format-1 letter that
/// carries one of the content headers W3-B4 moved into front matter (the
/// header is not read; the fact belongs in the front matter) or for bytes
/// that are not a mail message, `LetterError::UnknownKind` for a missing or
/// unknown kind, and the body's own parse error otherwise.
pub fn of_message(raw: &[u8], handle: &str) -> Result<Letter, LetterError> {
    let headers = read_headers(raw);
    let Some(format) = headers.format else {
        let kind = headers
            .kind
            .map(|kind| kind.to_lowercase())
            .filter(|kind| !kind.is_empty())
            .unwrap_or_else(|| "mail".to_string());
        return Err(LetterError::Legacy {
            handle: handle.to_string(),
            kind,
        });
    };
    if format != FORMAT_VERSION {
        return Err(LetterError::UnknownFormat(format));
    }
    if let Some(header) = headers.moved {
        return Err(LetterError::Invalid(format!(
            "letter {handle} carries `{header}` as a header: a format-1 letter \
             states that fact in its front matter, and the header is not read"
        )));
    }
    let kind = LetterKind::from_headers(headers.kind.as_deref(), headers.event.as_deref())?;
    let body = body_text(raw, handle)?;
    parse_kind(&kind, &body)
}

#[cfg(test)]
#[path = "message_test.rs"]
mod tests;
