//! Dispatch a request to a native in-process adapter.

use axum::http::{HeaderMap, Method};
use axum::response::Response;
use bytes::Bytes;

use super::codex;
use super::config::ProviderConfig;
use super::grok;
use super::kimi;
use super::route::Action;
use super::usage::Usage;
use crate::VivariumError;

/// Run the configured native adapter.
///
/// # Errors
///
/// Returns a configuration error when the adapter name is missing or unknown.
#[allow(clippy::too_many_arguments)]
pub async fn handle(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    action: &Action,
    method: Method,
    query: Option<&str>,
    headers: HeaderMap,
    body: Bytes,
    request_id: &str,
) -> Result<(Response, Usage), VivariumError> {
    match provider.adapter.as_deref() {
        Some("kimi-coding" | "openai-passthrough") => {
            kimi::handle(
                client, provider, action, method, query, headers, body, request_id,
            )
            .await
        }
        Some("xai-grok") => {
            grok::handle(
                client, provider, action, method, query, headers, body, request_id,
            )
            .await
        }
        Some("chatgpt-codex") => {
            codex::handle(client, provider, action, method, headers, body, request_id).await
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
        Some("xai-grok") => grok::credential_state(provider),
        Some("chatgpt-codex") => codex::credential_state(provider),
        _ => "unknown".into(),
    }
}

/// Health status for native adapters (`ok`, `missing`, `credential_dead`).
#[must_use]
pub fn health_status(provider: &ProviderConfig) -> &'static str {
    match provider.adapter.as_deref() {
        Some("kimi-coding" | "openai-passthrough") => kimi::health_status(provider),
        Some("xai-grok") => grok::health_status(provider),
        Some("chatgpt-codex") => codex::health_status(provider),
        _ => "missing",
    }
}
