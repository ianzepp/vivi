//! Reading a stored letter: the text body of an RFC 822 message, and the
//! front matter of a need.
//!
//! The console depends on no mail parser. A stored letter is a plain text
//! message made by vivi's own composer, so this reads what that composer can
//! write: headers, a transfer encoding of 7bit, 8bit, quoted-printable or
//! base64, and one level of multipart for the first text part. Anything else
//! comes out as the raw text after the headers.

use serde::Deserialize;
use vivi_core::pump::envelope::{self, Validate};

/// A letter's headers and decoded text body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Letter {
    headers: Vec<(String, String)>,
    pub body: String,
}

impl Letter {
    /// The first header named `name`, ignoring case.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// True for a lifecycle event letter (`done`, `marked`, ...): a record of
    /// a state change, not a message.
    #[must_use]
    pub fn is_event(&self) -> bool {
        self.header("X-Vivi-Event")
            .is_some_and(|value| !value.trim().is_empty())
    }
}

/// Parse raw message bytes.
#[must_use]
pub fn parse(raw: &[u8]) -> Letter {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = split_head(&text);
    let headers = unfold(head);
    let mut letter = Letter {
        headers,
        body: String::new(),
    };
    letter.body = text_of(&letter, body);
    letter
}

fn split_head(text: &str) -> (&str, &str) {
    for (marker, skip) in [("\r\n\r\n", 4), ("\n\n", 2)] {
        if let Some(at) = text.find(marker) {
            return (&text[..at], &text[at + skip..]);
        }
    }
    (text, "")
}

fn unfold(head: &str) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in head.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = headers.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    headers
}

fn text_of(letter: &Letter, body: &str) -> String {
    let content_type = letter.header("Content-Type").unwrap_or("text/plain");
    if content_type.to_ascii_lowercase().starts_with("multipart/") {
        return multipart_text(content_type, body);
    }
    let encoding = letter
        .header("Content-Transfer-Encoding")
        .unwrap_or("7bit")
        .to_ascii_lowercase();
    decode(&encoding, body)
}

fn decode(encoding: &str, body: &str) -> String {
    match encoding {
        "quoted-printable" => quoted_printable(body),
        "base64" => base64(body),
        _ => body.replace("\r\n", "\n"),
    }
}

/// The first part of a multipart body that is text (or has no type).
fn multipart_text(content_type: &str, body: &str) -> String {
    let Some(boundary) = parameter(content_type, "boundary") else {
        return body.to_string();
    };
    let marker = format!("--{boundary}");
    for part in body.split(&marker).skip(1) {
        if part.starts_with("--") {
            break;
        }
        let part = part.trim_start_matches(['\r', '\n']);
        let inner = parse(part.as_bytes());
        let kind = inner.header("Content-Type").unwrap_or("text/plain");
        if kind.to_ascii_lowercase().starts_with("text/plain") {
            // The line break before a boundary belongs to the boundary.
            return inner
                .body
                .strip_suffix('\n')
                .unwrap_or(&inner.body)
                .to_string();
        }
    }
    String::new()
}

fn parameter(header: &str, name: &str) -> Option<String> {
    header.split(';').skip(1).find_map(|part| {
        let (key, value) = part.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().trim_matches('"').to_string())
    })
}

fn quoted_printable(body: &str) -> String {
    let mut bytes: Vec<u8> = Vec::with_capacity(body.len());
    let lines: Vec<&str> = body.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let (line, soft) = match line.strip_suffix('=') {
            Some(rest) => (rest, true),
            None => (*line, false),
        };
        let raw = line.as_bytes();
        let mut at = 0;
        while at < raw.len() {
            match (raw[at], hex_pair(raw.get(at + 1..at + 3))) {
                (b'=', Some(byte)) => {
                    bytes.push(byte);
                    at += 3;
                }
                (other, _) => {
                    bytes.push(other);
                    at += 1;
                }
            }
        }
        if !soft && index + 1 < lines.len() {
            bytes.push(b'\n');
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn hex_pair(pair: Option<&[u8]>) -> Option<u8> {
    let pair = pair?;
    let high = char::from(pair[0]).to_digit(16)?;
    let low = char::from(pair[1]).to_digit(16)?;
    // Two hex digits are at most 255.
    Some(u8::try_from(high * 16 + low).unwrap_or(0))
}

fn base64(body: &str) -> String {
    let mut bytes: Vec<u8> = Vec::with_capacity(body.len() / 4 * 3);
    let (mut buffer, mut bits) = (0_u32, 0_u32);
    for ch in body.chars() {
        let Some(value) = sextet(ch) else {
            continue;
        };
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push(u8::try_from((buffer >> bits) & 0xFF).unwrap_or(0));
        }
    }
    String::from_utf8_lossy(&bytes).replace("\r\n", "\n")
}

fn sextet(ch: char) -> Option<u32> {
    match ch {
        'A'..='Z' => Some(u32::from(ch) - u32::from('A')),
        'a'..='z' => Some(u32::from(ch) - u32::from('a') + 26),
        '0'..='9' => Some(u32::from(ch) - u32::from('0') + 52),
        '+' => Some(62),
        '/' => Some(63),
        _ => None,
    }
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
