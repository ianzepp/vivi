//! Dispatch a request to a native in-process adapter.

use axum::http::{HeaderMap, Method};
use axum::response::Response;
use bytes::Bytes;

use super::codex;
use super::config::ProviderConfig;
use super::kimi;
use super::route::Action;
use super::usage::Usage;
use crate::VivariumError;

/// Run the configured native adapter.
///
/// # Errors
///
/// Returns a configuration error when the adapter name is missing or unknown.
pub async fn handle(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    action: &Action,
    method: Method,
    query: Option<&str>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(Response, Usage), VivariumError> {
    match provider.adapter.as_deref() {
        Some("kimi-coding" | "openai-passthrough") => {
            kimi::handle(client, provider, action, method, query, headers, body).await
        }
        Some("chatgpt-codex") => {
            codex::handle(client, provider, action, method, headers, body).await
        }
        _ => Err(VivariumError::Config(
            "native provider is missing a supported adapter".into(),
        )),
    }
}

/// Credential expiry label (never the token).
#[must_use]
pub fn credential_state(provider: &ProviderConfig) -> String {
    match provider.adapter.as_deref() {
        Some("kimi-coding" | "openai-passthrough") => kimi::credential_state(provider),
        Some("chatgpt-codex") => codex::credential_state(provider),
        _ => "unknown".into(),
    }
}
