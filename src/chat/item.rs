//! Deterministic mail rendering of room items.
//!
//! A pulled item becomes a real `.eml` blob. The bytes are a pure function of
//! `(room_id, record)`: no clock, no local identity, no cursor. Every store
//! therefore derives the same `content_id` for the same item, and a resync
//! rebuilds an evicted item byte-identically. The local recipient lives in
//! the `messages.account` column, never in the bytes.
//!
//! Everything in a record is foreign input: labels, subjects, and ids are
//! flattened to header-safe text here, and the exact label stays in the
//! `X-Vivi-Chat-From` header.

use chrono::DateTime;

use crate::chat::b64;
use crate::chat::client::ChatMessage;
use crate::error::VivariumError;

/// Remote-binding provider for room items.
pub const PROVIDER: &str = "vivi-room";

/// Fixed `remote_uidvalidity` for room bindings: a room id carries 125 bits
/// of entropy, so it never needs a validity epoch (D4).
pub const UIDVALIDITY: u32 = 1;

const MAX_SUBJECT_CHARS: usize = 512;
const MAX_REFS: usize = 32;
const MAX_ID_CHARS: usize = 200;

/// The mail kind a wire `kind` files under, if it is one the client knows.
/// Unknown future kinds degrade to plain mail instead of failing a pull.
#[must_use]
pub fn work_kind(kind: Option<&str>) -> Option<&'static str> {
    match kind?.to_ascii_lowercase().as_str() {
        "need" => Some("need"),
        "task" => Some("task"),
        "want" => Some("want"),
        _ => None,
    }
}

/// Local role an item files under: `sent` for the seat's own sends, the work
/// folder for need/task/want, and `inbox` otherwise (D6).
#[must_use]
pub fn role_for(kind: Option<&str>, own: bool) -> &'static str {
    if own {
        return "sent";
    }
    match work_kind(kind) {
        Some("need") => "needs",
        Some("task") => "tasks",
        Some("want") => "wants",
        _ => "inbox",
    }
}

/// Render one room record as an `.eml`.
///
/// # Errors
/// Returns an error when `created_at` is not RFC 3339; the caller skips and
/// reports such an item rather than inventing a date.
pub fn render_eml(room_id: &str, message: &ChatMessage) -> Result<String, VivariumError> {
    let date = DateTime::parse_from_rfc3339(&message.created_at).map_err(|_| {
        VivariumError::Message(format!(
            "item {} has an invalid created_at",
            clip(&single_line(&message.message_id), 64)
        ))
    })?;
    let domain = format!("{room_id}.vivi-room");
    let mut headers = vec![
        format!("From: {}@{domain}", address_local(&message.from)),
        format!("To: {}", to_address(message.to.as_deref(), &domain)),
        format!("Subject: {}", subject_header(&message.subject)),
        format!("Date: {}", date.to_rfc2822()),
        format!("Message-ID: {}", id_header(&message.message_id, &domain)),
    ];
    if let Some(parent) = message.in_reply_to.as_deref().filter(|p| !p.is_empty()) {
        headers.push(format!("In-Reply-To: {}", id_header(parent, &domain)));
    }
    let refs: Vec<String> = message
        .refs
        .iter()
        .filter(|r| !r.is_empty())
        .take(MAX_REFS)
        .map(|r| id_header(r, &domain))
        .collect();
    if !refs.is_empty() {
        headers.push(format!("References: {}", refs.join(" ")));
    }
    if let Some(kind) = work_kind(message.kind.as_deref()) {
        headers.push(format!("X-Vivi-Kind: {kind}"));
    }
    headers.push(format!("X-Vivi-Chat-Room: {room_id}"));
    headers.push(format!(
        "X-Vivi-Chat-From: {}",
        clip(&single_line(&message.from), 128)
    ));
    headers.push(format!(
        "X-Vivi-Chat-Fingerprint: {}",
        clip(&single_line(&message.fingerprint), 64)
    ));
    headers.push("MIME-Version: 1.0".into());
    headers.push("Content-Type: text/plain; charset=utf-8".into());
    headers.push("Content-Transfer-Encoding: 8bit".into());
    Ok(format!("{}\n\n{}", headers.join("\n"), message.body))
}

fn to_address(to: Option<&str>, domain: &str) -> String {
    match to {
        Some(label) => format!("{}@{domain}", address_local(label)),
        None => format!("room@{domain}"),
    }
}

/// Header-safe `<local@domain>` form of a Message-ID (legacy ids such as
/// `hex@fingerprint` are flattened, not rejected).
fn id_header(id: &str, domain: &str) -> String {
    let local: String = id
        .chars()
        .take(MAX_ID_CHARS)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("<{local}@{domain}>")
}

/// Mailbox local part from a seat label: keep `[A-Za-z0-9_-]`, keep a `.`
/// only between kept characters, flatten the rest.
fn address_local(label: &str) -> String {
    let mut out = String::new();
    for c in label.chars() {
        let kept = c.is_ascii_alphanumeric() || matches!(c, '_' | '-');
        if kept {
            out.push(c);
        } else if c == '.' && !out.is_empty() && !out.ends_with('.') {
            out.push('.');
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches(|c| c == '.' || c == '-');
    if trimmed.is_empty() {
        "seat".into()
    } else {
        trimmed.into()
    }
}

/// Collapse control characters and line breaks to single spaces.
fn single_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

fn clip(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// Subject header text: single line, capped, and RFC 2047 encoded when it is
/// not plain ASCII.
fn subject_header(subject: &str) -> String {
    let line = clip(&single_line(subject), MAX_SUBJECT_CHARS);
    if line.is_empty() {
        return "(no subject)".into();
    }
    if line.is_ascii() {
        return line;
    }
    format!("=?UTF-8?B?{}?=", b64::encode(line.as_bytes()))
}

#[cfg(test)]
#[path = "item_test.rs"]
mod tests;
