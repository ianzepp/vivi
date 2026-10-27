//! Outbound message composition for mailspace delivery.
//!
//! The mailspace sends identity-to-identity mail through its own store, so it
//! needs the composition surface, the Message-ID normalizer used by storage,
//! and enough of a parser to render a stored message back to a reader.

mod compose;
mod render;

pub use compose::{
    ComposeDraft, FileAttachment, ReplyDraft, auto_html_body, build_compose_draft,
    build_compose_draft_with_attachments, build_reply, build_reply_template, replace_from_header,
    validate_message_headers,
};
pub use render::{render_message, to_json_message};

/// Normalize a Message-ID header value for storage lookup.
///
/// Strips the surrounding angle brackets and lowercases the result. Returns
/// `None` when nothing is left.
#[must_use]
pub fn normalize_message_id(message_id: &str) -> Option<String> {
    let trimmed = message_id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_ascii_lowercase())
    }
}
