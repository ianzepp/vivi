//! Shared OAuth refresh-token wire call and response helpers.

use serde_json::Value;

use super::dead::{self, RefreshClass};

/// POST a `refresh_token` grant as a form and return the JSON payload.
///
/// `label` names the adapter in logs. The response body is never logged.
///
/// # Errors
///
/// Returns [`RefreshClass::Refused`] for a definitive refusal (HTTP 400/401)
/// and [`RefreshClass::Transient`] for network, 429, 5xx, or a non-JSON body.
pub async fn post_refresh(
    client: &reqwest::Client,
    token_url: &str,
    form: &[(&str, &str)],
    label: &str,
) -> Result<Value, RefreshClass> {
    let response = match client.post(token_url).form(form).send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, adapter = label, "token endpoint unreachable");
            return Err(RefreshClass::Transient);
        }
    };
    match dead::classify_status(response.status()) {
        RefreshClass::Ok => response.json().await.map_err(|error| {
            tracing::warn!(%error, adapter = label, "token refresh returned non-JSON");
            RefreshClass::Transient
        }),
        RefreshClass::Refused => Err(RefreshClass::Refused),
        RefreshClass::Transient => {
            tracing::warn!(status = %response.status(), adapter = label, "token refresh transient");
            Err(RefreshClass::Transient)
        }
    }
}

/// `expires_in` seconds from a token response (`as_i64` also covers `u64`).
#[must_use]
pub fn expires_in_secs(payload: &Value) -> Option<i64> {
    payload.get("expires_in")?.as_i64()
}
