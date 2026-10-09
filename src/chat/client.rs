//! Chat server client for the frozen v1 wire contract: challenge → sign →
//! join. Tokens are minted per invocation and never persisted.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::chat::b64;
use crate::chat::identity::ChatIdentity;
use crate::error::VivariumError;

/// Request timeout for chat server calls.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Receipt from a successful seat join.
#[derive(Debug, Clone)]
pub struct JoinReceipt {
    pub room_id: String,
    pub seat: u32,
    pub label: String,
    pub token_expires_at: String,
    pub fingerprint: String,
}

#[derive(Debug, Deserialize)]
struct ChallengeResponse {
    challenge_id: String,
    challenge: String,
}

#[derive(Debug, Deserialize)]
struct JoinResponse {
    seat: i64,
    label: String,
    token_expires_at: String,
    fingerprint: String,
}

/// Normalize a room id per the contract (L→l, I→i, O→o, lowercase) and
/// reject ids that are not canonical Crockford-base32 grouped form.
pub fn normalize_room_id(raw: &str) -> Result<String, VivariumError> {
    let normalized: String = raw
        .chars()
        .map(|c| match c {
            'L' => 'l',
            'I' => 'i',
            'O' => 'o',
            other => other.to_ascii_lowercase(),
        })
        .collect();
    let canonical = normalized
        .chars()
        .all(|c| c.is_ascii_digit() || ('a'..='z').contains(&c) && !"iou".contains(c) || c == '-');
    let grouped = normalized.split('-').count() == 5 && !normalized.starts_with('-');
    if !canonical || !grouped {
        return Err(VivariumError::Message(format!(
            "'{raw}' is not a canonical room id (expected 5 groups of Crockford base32)"
        )));
    }
    Ok(normalized)
}

/// Default seat label: `<short-hostname>/<project-dir-name>`.
#[must_use]
pub fn default_label(project_root: &std::path::Path) -> String {
    let host = crate::role_status::local_host_name().unwrap_or_else(|| "localhost".into());
    let short = host.split('.').next().unwrap_or("localhost");
    let project = project_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    format!("{short}/{project}")
}

/// Complete the challenge → sign → join handshake for `room_id`.
///
/// # Errors
/// Returns a friendly error when the server is unreachable, rejects the
/// room id, the room is full, or the label is taken.
pub fn join_room(
    server: &str,
    raw_room_id: &str,
    identity: &ChatIdentity,
    label: &str,
) -> Result<JoinReceipt, VivariumError> {
    let room_id = normalize_room_id(raw_room_id)?;
    let pem = identity.public_key_pem();
    let base = server.trim_end_matches('/');

    let challenge_url = format!("{base}/v1/rooms/{room_id}/challenge");
    let (status, body) = post_json(&challenge_url, &json!({ "public_key_pem": pem }))?;
    let code = error_code(&body);
    if status != 200 {
        return Err(join_error(status, &code, &body, raw_room_id, label));
    }
    let challenge: ChallengeResponse = parse_body(&body)?;
    let bytes = b64::decode(&challenge.challenge)
        .ok_or_else(|| VivariumError::Message("server returned an undecodable challenge".into()))?;
    if bytes.len() != 32 {
        return Err(VivariumError::Message(format!(
            "server challenge is {} bytes, expected 32",
            bytes.len()
        )));
    }
    let signature = b64::encode(&identity.sign(&bytes));

    let join_url = format!("{base}/v1/rooms/{room_id}/join");
    let (status, body) = post_json(
        &join_url,
        &json!({
            "challenge_id": challenge.challenge_id,
            "signature_base64": signature,
            "public_key_pem": pem,
            "label": label,
        }),
    )?;
    let code = error_code(&body);
    if status != 200 && status != 201 {
        return Err(join_error(status, &code, &body, raw_room_id, label));
    }
    let joined: JoinResponse = parse_body(&body)?;
    Ok(JoinReceipt {
        room_id,
        seat: joined.seat as u32,
        label: joined.label,
        token_expires_at: joined.token_expires_at,
        fingerprint: joined.fingerprint,
    })
}

fn parse_body<T: for<'de> Deserialize<'de>>(body: &Value) -> Result<T, VivariumError> {
    serde_json::from_value(body.clone())
        .map_err(|_| VivariumError::Message("server returned an unexpected response".into()))
}

fn error_code(body: &Value) -> String {
    body.get("error")
        .and_then(|e| e.get("code"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn join_error(status: u16, code: &str, body: &Value, room_id: &str, label: &str) -> VivariumError {
    let detail = body
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    match code {
        "room_not_found" => VivariumError::Message(format!("room '{room_id}' not found")),
        "seat_full" => VivariumError::Message(format!("room '{room_id}' is full: no open seat")),
        "label_taken" => VivariumError::Message(format!(
            "label '{label}' is already taken in room '{room_id}'; retry with --label"
        )),
        "invalid_room_id" => {
            VivariumError::Message(format!("server rejected room id '{room_id}' as invalid"))
        }
        "challenge_failed" => VivariumError::Message(
            "server rejected the join proof (challenge expired or signature invalid)".into(),
        ),
        other => VivariumError::Message(format!(
            "chat server error (HTTP {status}, {other}): {detail}"
        )),
    }
}

/// Blocking JSON POST, mirroring the judgment provider's thread + runtime
/// pattern so callers stay synchronous.
fn post_json(url: &str, payload: &Value) -> Result<(u16, Value), VivariumError> {
    let url = url.to_string();
    let payload = payload.clone();
    let handle = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| VivariumError::Other(format!("failed to build runtime: {e}")))?;
        runtime.block_on(async move {
            let client = reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .map_err(|e| VivariumError::Other(format!("failed to build client: {e}")))?;
            let response = client.post(&url).json(&payload).send().await.map_err(|e| {
                if e.is_timeout() {
                    VivariumError::Message(format!(
                        "chat server timed out after {}ms",
                        TIMEOUT.as_millis()
                    ))
                } else {
                    VivariumError::Message(format!("chat server unreachable: {e}"))
                }
            })?;
            let status = response.status().as_u16();
            let body = response.json::<Value>().await.map_err(|e| {
                VivariumError::Message(format!("invalid chat server response: {e}"))
            })?;
            Ok((status, body))
        })
    });
    handle
        .join()
        .map_err(|_| VivariumError::Message("chat client thread panicked".into()))?
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
