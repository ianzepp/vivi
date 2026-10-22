//! Session-affinity header for `openai-passthrough` providers that need one.
//!
//! Some upstreams (OpenCode Go) reject a request without a session header
//! (`MissingSessionID`). A provider opts in with `session_header = "<name>"`.
//! A client-supplied value is forwarded unchanged. Otherwise the proxy adds
//! one derived from the request body only, never from a credential: stable
//! for one conversation (so the upstream can route it to the same cache),
//! random when the body offers nothing to derive it from.

use axum::http::{HeaderMap, HeaderName, HeaderValue};
use rand_core::{OsRng, RngCore};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::config::ProviderConfig;

/// Headers that a session header must never name.
const FORBIDDEN: [&str; 12] = [
    "authorization",
    "host",
    "content-length",
    "content-type",
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// True when `name` is a lowercase HTTP header name that is safe to use for a session id.
#[must_use]
pub fn valid_header_name(name: &str) -> bool {
    let Ok(parsed) = HeaderName::from_bytes(name.as_bytes()) else {
        return false;
    };
    parsed.as_str() == name && !FORBIDDEN.contains(&name)
}

/// The header to add to an outbound request, or `None` when the provider
/// has no `session_header` or the client already sent one.
#[must_use]
pub fn header_to_add(
    provider: &ProviderConfig,
    inbound: &HeaderMap,
    body: &[u8],
) -> Option<(HeaderName, HeaderValue)> {
    let configured = provider.session_header.as_deref()?;
    let name = HeaderName::from_bytes(configured.as_bytes()).ok()?;
    if inbound.contains_key(&name) {
        return None;
    }
    let value = HeaderValue::from_str(&session_id(body)).ok()?;
    Some((name, value))
}

/// Session id for a request body: stable per conversation when derivable,
/// otherwise random.
#[must_use]
pub fn session_id(body: &[u8]) -> String {
    match conversation_key(body) {
        Some(key) => format!("ses_{}", &hex::encode(Sha256::digest(key.as_bytes()))[..32]),
        None => random_id(),
    }
}

/// Model plus the first non-system message. The system prompt is shared by
/// every conversation of one client, so it would pin all of them to one
/// route; the first user turn is what stays constant as a conversation grows.
fn conversation_key(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let model = value.get("model").and_then(Value::as_str)?;
    let messages = value.get("messages").and_then(Value::as_array)?;
    let first = messages
        .iter()
        .find(|message| !is_system(message))
        .or_else(|| messages.first())?;
    let content = first.get("content")?;
    if content.is_null() {
        return None;
    }
    Some(format!("{model}\0{content}"))
}

fn is_system(message: &Value) -> bool {
    matches!(
        message.get("role").and_then(Value::as_str),
        Some("system" | "developer")
    )
}

fn random_id() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    format!("ses_{}", hex::encode(bytes))
}
