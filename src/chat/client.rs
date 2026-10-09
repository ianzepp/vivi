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
struct TokenResponse {
    token: String,
}

#[derive(Debug, Deserialize)]
struct AckResponse {
    message_id: String,
    cursor: i64,
    #[serde(default)]
    duplicate: bool,
}

#[derive(Debug, Deserialize)]
struct PollResponse {
    #[serde(default)]
    messages: Vec<ChatMessage>,
    next_cursor: u64,
}

/// Ack for one delivered message; `duplicate` marks a replayed Message-ID.
#[derive(Debug, Clone)]
pub struct MessageAck {
    pub message_id: String,
    pub cursor: u64,
    pub duplicate: bool,
}

/// One message as polled from a room (origin label + fingerprint included).
#[derive(Debug, Clone, Deserialize)]
pub struct ChatMessage {
    pub message_id: String,
    pub cursor: u64,
    pub from: String,
    pub fingerprint: String,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub subject: String,
    pub body: String,
    pub created_at: String,
    /// `need`, `task`, or `want` for work items; absent for plain chat (R8).
    #[serde(default)]
    pub kind: Option<String>,
    /// Message-ID this item answers (R7). Never resolved server-side.
    #[serde(default)]
    pub in_reply_to: Option<String>,
    /// Message-IDs this item cites (R7).
    #[serde(default)]
    pub refs: Vec<String>,
}

/// One poll page: messages after the cursor plus the following cursor.
#[derive(Debug, Clone)]
pub struct PollPage {
    pub messages: Vec<ChatMessage>,
    pub next_cursor: u64,
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

/// Default seat label: `<short-hostname>.<project-dir-name>`, sanitized to
/// the contract R4 label charset (the label doubles as a vivi identity
/// local-part on the server, so `/` and friends are illegal).
#[must_use]
pub fn default_label(project_root: &std::path::Path) -> String {
    let host = crate::role_status::local_host_name().unwrap_or_else(|| "localhost".into());
    let short = host.split('.').next().unwrap_or("localhost");
    let project = project_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    let legal = |part: &str| {
        part.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>()
    };
    format!("{}.{}", legal(short), legal(project))
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
    let signature = sign_challenge(identity, &challenge.challenge)?;

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

/// Decode and sign an issued challenge (32 bytes), returning base64.
fn sign_challenge(identity: &ChatIdentity, challenge_b64: &str) -> Result<String, VivariumError> {
    let bytes = b64::decode(challenge_b64)
        .ok_or_else(|| VivariumError::Message("server returned an undecodable challenge".into()))?;
    if bytes.len() != 32 {
        return Err(VivariumError::Message(format!(
            "server challenge is {} bytes, expected 32",
            bytes.len()
        )));
    }
    Ok(b64::encode(&identity.sign(&bytes)))
}

/// Create a room via `POST /v1/rooms`; the creator key auto-binds seat 1.
///
/// # Errors
/// Returns a friendly error when the server is unreachable, rejects the
/// capacity (2..8), or cannot bind the creator seat.
pub fn create_room(
    server: &str,
    seats: Option<u32>,
    label: Option<&str>,
    identity: &ChatIdentity,
) -> Result<JoinReceipt, VivariumError> {
    let base = server.trim_end_matches('/');
    let (status, body) = post_json(
        &format!("{base}/v1/rooms"),
        &json!({
            "seats": seats,
            "label": label,
            "public_key_pem": identity.public_key_pem(),
        }),
    )?;
    let code = error_code(&body);
    if status != 200 && status != 201 {
        return Err(join_error(
            status,
            &code,
            &body,
            "new room",
            label.unwrap_or_default(),
        ));
    }
    #[derive(Deserialize)]
    struct CreateResponse {
        room_id: String,
        seat: i64,
        label: String,
        token_expires_at: String,
        fingerprint: String,
    }
    let created: CreateResponse = parse_body(&body)?;
    Ok(JoinReceipt {
        room_id: created.room_id,
        seat: created.seat as u32,
        label: created.label,
        token_expires_at: created.token_expires_at,
        fingerprint: created.fingerprint,
    })
}

/// Mint a fresh per-seat bearer: fingerprint challenge → sign → auth/token.
/// The raw token exists only for the current invocation.
///
/// # Errors
/// Returns a friendly error when the server is unreachable, the room is
/// unknown, or the seat proof is rejected.
pub fn mint_bearer(
    server: &str,
    raw_room_id: &str,
    identity: &ChatIdentity,
) -> Result<String, VivariumError> {
    let room_id = normalize_room_id(raw_room_id)?;
    let base = server.trim_end_matches('/');
    let (status, body) = post_json(
        &format!("{base}/v1/rooms/{room_id}/challenge"),
        &json!({ "fingerprint": identity.wire_fingerprint() }),
    )?;
    let code = error_code(&body);
    if status != 200 {
        return Err(token_error(status, &code, &body, raw_room_id));
    }
    let challenge: ChallengeResponse = parse_body(&body)?;
    let signature = sign_challenge(identity, &challenge.challenge)?;
    let (status, body) = post_json(
        &format!("{base}/v1/rooms/{room_id}/auth/token"),
        &json!({
            "challenge_id": challenge.challenge_id,
            "signature_base64": signature,
        }),
    )?;
    let code = error_code(&body);
    if status != 200 {
        return Err(token_error(status, &code, &body, raw_room_id));
    }
    let token: TokenResponse = parse_body(&body)?;
    Ok(token.token)
}

/// POST one message under a bearer. `message_id` rides the Message-ID
/// header for server-side idempotency; origin headers must match the seat.
///
/// # Errors
/// Returns a friendly error (mapped from contract codes) on any non-2xx.
pub fn post_message(
    server: &str,
    raw_room_id: &str,
    token: &str,
    label: &str,
    fingerprint_short: &str,
    message_id: &str,
    payload: &Value,
) -> Result<MessageAck, VivariumError> {
    let room_id = normalize_room_id(raw_room_id)?;
    let url = format!(
        "{}/v1/rooms/{room_id}/messages",
        server.trim_end_matches('/')
    );
    let headers = origin_headers(token, label, fingerprint_short, Some(message_id));
    let (status, body) = request_json("POST", &url, Some(payload), &headers)?;
    let code = error_code(&body);
    if status != 200 && status != 201 {
        return Err(send_error(status, &code, &body));
    }
    let ack: AckResponse = parse_body(&body)?;
    Ok(MessageAck {
        message_id: ack.message_id,
        cursor: ack.cursor.max(0) as u64,
        duplicate: ack.duplicate,
    })
}

/// Poll one page of messages after `since` (origin headers ride the request).
///
/// # Errors
/// Returns a friendly error when the poll fails or is unauthorized.
pub fn poll_messages(
    server: &str,
    raw_room_id: &str,
    token: &str,
    label: &str,
    fingerprint_short: &str,
    since: u64,
    wait_secs: u64,
) -> Result<PollPage, VivariumError> {
    let room_id = normalize_room_id(raw_room_id)?;
    let url = format!(
        "{}/v1/rooms/{room_id}/messages?since={since}&wait={wait_secs}",
        server.trim_end_matches('/')
    );
    let headers = origin_headers(token, label, fingerprint_short, None);
    // The long-poll hold may outlive the default request timeout.
    let timeout = Duration::from_secs(wait_secs + 10);
    let (status, body) = request_json_with_timeout("GET", &url, None, &headers, timeout)?;
    let code = error_code(&body);
    if status != 200 {
        return Err(send_error(status, &code, &body));
    }
    let page: PollResponse = parse_body(&body)?;
    Ok(PollPage {
        messages: page.messages,
        next_cursor: page.next_cursor,
    })
}

/// Bearer + origin header set shared by send and poll.
fn origin_headers(
    token: &str,
    label: &str,
    fingerprint_short: &str,
    message_id: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut headers = vec![
        ("Authorization", format!("Bearer {token}")),
        ("X-Vivi-Chat-From", label.to_string()),
        ("X-Vivi-Chat-Fingerprint", fingerprint_short.to_string()),
    ];
    if let Some(message_id) = message_id {
        headers.push(("Message-ID", message_id.to_string()));
    }
    headers
}

fn token_error(status: u16, code: &str, body: &Value, room_id: &str) -> VivariumError {
    match code {
        "room_not_found" => VivariumError::Message(format!("room '{room_id}' not found")),
        "challenge_failed" | "unauthorized" => VivariumError::Message(format!(
            "chat server refused the bearer mint ({code}); rejoin with vivi chat join"
        )),
        other => VivariumError::Message(format!(
            "chat server error (HTTP {status}, {other}): {}",
            error_detail(body)
        )),
    }
}

fn send_error(status: u16, code: &str, body: &Value) -> VivariumError {
    match code {
        "unauthorized" => VivariumError::Message(
            "chat server rejected the send (bearer expired or origin mismatch); message stays queued"
                .into(),
        ),
        "rate_limited" => VivariumError::Message(
            "chat server rate limit hit (30 sends/min/seat); message stays queued".into(),
        ),
        "body_too_large" => {
            VivariumError::Message("message body over the 64 KiB chat cap".into())
        }
        "room_not_found" => VivariumError::Message("chat room not found on the server".into()),
        other => VivariumError::Message(format!(
            "chat server error (HTTP {status}, {other}): {}",
            error_detail(body)
        )),
    }
}

fn error_detail(body: &Value) -> String {
    body.get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
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
    request_json("POST", url, Some(payload), &[])
}

/// Blocking JSON request with extra headers, same thread + runtime pattern.
fn request_json(
    method: &str,
    url: &str,
    payload: Option<&Value>,
    headers: &[(&'static str, String)],
) -> Result<(u16, Value), VivariumError> {
    request_json_with_timeout(method, url, payload, headers, TIMEOUT)
}

/// `request_json` with a caller-chosen timeout (long-poll holds exceed the
/// default).
fn request_json_with_timeout(
    method: &str,
    url: &str,
    payload: Option<&Value>,
    headers: &[(&'static str, String)],
    timeout: Duration,
) -> Result<(u16, Value), VivariumError> {
    let url = url.to_string();
    let payload = payload.cloned();
    let method = method.to_string();
    let headers = headers.to_vec();
    let handle = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| VivariumError::Other(format!("failed to build runtime: {e}")))?;
        runtime.block_on(async move {
            let client = reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .map_err(|e| VivariumError::Other(format!("failed to build client: {e}")))?;
            let method = reqwest::Method::from_bytes(method.as_bytes())
                .map_err(|e| VivariumError::Other(format!("invalid HTTP method: {e}")))?;
            let mut request = client.request(method, &url);
            for (name, value) in &headers {
                request = request.header(*name, value.as_str());
            }
            if let Some(payload) = payload.as_ref() {
                request = request.json(payload);
            }
            let response = request.send().await.map_err(|e| {
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
