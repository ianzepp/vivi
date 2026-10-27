//! Normalized RFC identity for the hosted pipeline: the form dependent
//! events are woken by.
//!
//! Every Message-ID a pass reads is carried in exactly one shape — trimmed,
//! without angle brackets, lowercased — so a waiting content id and the
//! letter that wakes it meet on the same value.

use crate::message::normalize_message_id;

/// Normalize a raw RFC Message-ID, or `None` when nothing remains after
/// trimming. This is the identity a waiting content id is woken by.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn normalize(raw: &str) -> Option<String> {
    normalize_message_id(raw)
}
