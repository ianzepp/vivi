//! Machine-key sign-in for `/v1/mcp`.
//!
//! A machine holds one ed25519 key (`$VIVI_HOME/keys/machine.ed25519`). To
//! sign in it asks for a challenge, signs `MESSAGE_PREFIX || nonce`, and gets
//! a session token that lasts [`SESSION_SECS`]. The token is a bearer on
//! `/v1/mcp`; the principal's grants are re-read on every request, so taking
//! a key or a grant away takes effect at once.
//!
//! Who a key is:
//! - `as_host: true` — the key must be listed in `$VIVI_HOME/server/host_keys`
//!   (one base64 public key per line, `#` comments). It signs in as `host`.
//! - otherwise — the principal that holds the key (`machine_add`).
//!
//! Challenges and sessions live in memory: a restart signs every machine
//! out, and the client signs in again.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::server::AppState;

/// What the machine signs: this prefix, then the raw nonce bytes.
pub const MESSAGE_PREFIX: &[u8] = b"vivi machine sign-in v1\n";
/// Session token prefix, so a log line shows what kind of bearer it was.
pub const TOKEN_PREFIX: &str = "vivims_";
const CHALLENGE_SECS: u64 = 120;
/// Live challenges one public key may hold.
const MAX_CHALLENGES_PER_KEY: usize = 8;
/// Live challenges the server holds in total.
const MAX_CHALLENGES_TOTAL: usize = 4096;
const SESSION_SECS: u64 = 3600;

struct Challenge {
    public_key: [u8; 32],
    nonce: [u8; 32],
    expires: Instant,
}

/// A live session.
#[derive(Clone)]
pub struct Session {
    pub principal: String,
    pub host: bool,
    expires: Instant,
}

static CHALLENGES: LazyLock<Mutex<HashMap<String, Challenge>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SESSIONS: LazyLock<Mutex<HashMap<String, Session>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `SHA256:<base64 no padding>` of the raw public key, as ssh prints it.
#[must_use]
pub fn fingerprint(public_key: &[u8]) -> String {
    let digest = Sha256::digest(public_key);
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
    )
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn random32() -> Result<[u8; 32], getrandom::Error> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw)?;
    Ok(raw)
}

/// The refusal for a failed read of the OS random source.
fn entropy_failed(error: &getrandom::Error) -> Response {
    tracing::error!("the OS random source failed: {error}");
    refuse(
        StatusCode::INTERNAL_SERVER_ERROR,
        "the server cannot read the OS random source",
    )
}

fn refuse(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

#[derive(Deserialize)]
pub struct ChallengeRequest {
    public_key: String,
}

/// `POST /v1/auth/machine/challenge` `{public_key}` → `{challenge_id, nonce}`.
pub async fn challenge_handler(Json(req): Json<ChallengeRequest>) -> Response {
    let Some(public_key) = decode_key(&req.public_key) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "public_key must be base64 of 32 bytes",
        );
    };
    let (id, nonce) = match (random32(), random32()) {
        (Ok(id), Ok(nonce)) => (hex::encode(id), nonce),
        (Err(e), _) | (_, Err(e)) => return entropy_failed(&e),
    };
    let mut book = CHALLENGES.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    book.retain(|_, c| c.expires > now);
    if book.len() >= MAX_CHALLENGES_TOTAL {
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            &format!("too many live challenges: the server holds at most {MAX_CHALLENGES_TOTAL}"),
        );
    }
    if book.values().filter(|c| c.public_key == public_key).count() >= MAX_CHALLENGES_PER_KEY {
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            &format!("too many live challenges: at most {MAX_CHALLENGES_PER_KEY} per public key"),
        );
    }
    book.insert(
        id.clone(),
        Challenge {
            public_key,
            nonce,
            expires: now + Duration::from_secs(CHALLENGE_SECS),
        },
    );
    Json(json!({ "challenge_id": id, "nonce": b64().encode(nonce) })).into_response()
}

/// That `signature` signs the challenge's nonce with the challenge's key.
fn check_signature(challenge: &Challenge, signature: &str) -> Result<(), Response> {
    let Ok(sig_bytes) = b64().decode(signature.trim()) else {
        return Err(refuse(StatusCode::BAD_REQUEST, "signature must be base64"));
    };
    let Ok(signature) = Signature::from_slice(&sig_bytes) else {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "signature must be 64 bytes",
        ));
    };
    let Ok(key) = VerifyingKey::from_bytes(&challenge.public_key) else {
        return Err(refuse(StatusCode::BAD_REQUEST, "not an ed25519 public key"));
    };
    let mut message = MESSAGE_PREFIX.to_vec();
    message.extend_from_slice(&challenge.nonce);
    if key.verify(&message, &signature).is_err() {
        return Err(refuse(
            StatusCode::UNAUTHORIZED,
            "signature does not verify",
        ));
    }
    Ok(())
}

/// The principal a verified key signs in as, and whether it is the host:
/// the host when asked for and listed in `host_keys`, else the principal the
/// key was admitted under.
fn sign_in_principal(
    state: &AppState,
    encoded: &str,
    print: &str,
    as_host: bool,
) -> Result<(String, bool), Response> {
    if as_host {
        if !host_keys().iter().any(|k| k == encoded) {
            return Err(refuse(
                StatusCode::FORBIDDEN,
                &format!("{print} is not listed in the server's host_keys"),
            ));
        }
        return Ok((crate::server::mcp::server::HOST_PRINCIPAL.to_owned(), true));
    }
    match state.server_keys.machine_principal(encoded) {
        Some(principal) => Ok((principal, false)),
        None => Err(refuse(
            StatusCode::FORBIDDEN,
            &format!("{print} is not admitted on this server (host tool machine_add)"),
        )),
    }
}

#[derive(Deserialize)]
pub struct VerifyRequest {
    challenge_id: String,
    signature: String,
    #[serde(default)]
    as_host: bool,
}

/// `POST /v1/auth/machine/verify` `{challenge_id, signature, as_host?}` →
/// `{token, expires_in, principal, host, fingerprint}`.
pub async fn verify_handler(
    State(state): State<AppState>,
    Json(req): Json<VerifyRequest>,
) -> Response {
    let challenge = {
        let mut book = CHALLENGES.lock().unwrap_or_else(|e| e.into_inner());
        book.remove(&req.challenge_id)
    };
    let Some(challenge) = challenge.filter(|c| c.expires > Instant::now()) else {
        return refuse(StatusCode::UNAUTHORIZED, "unknown or expired challenge");
    };
    if let Err(refusal) = check_signature(&challenge, &req.signature) {
        return refusal;
    }
    let encoded = b64().encode(challenge.public_key);
    let print = fingerprint(&challenge.public_key);
    let (principal, host) = match sign_in_principal(&state, &encoded, &print, req.as_host) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let secret = match random32() {
        Ok(secret) => secret,
        Err(e) => return entropy_failed(&e),
    };
    let token = format!("{TOKEN_PREFIX}{}", hex::encode(secret));
    let digest = hex::encode(Sha256::digest(token.as_bytes()));
    let mut sessions = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    sessions.retain(|_, s| s.expires > now);
    sessions.insert(
        digest,
        Session {
            principal: principal.clone(),
            host,
            expires: now + Duration::from_secs(SESSION_SECS),
        },
    );
    tracing::info!("machine sign-in {print} as {principal}");
    Json(json!({
        "token": token,
        "expires_in": SESSION_SECS,
        "principal": principal,
        "host": host,
        "fingerprint": print,
    }))
    .into_response()
}

/// The live session a bearer names, if any.
#[must_use]
pub fn session(bearer: &str) -> Option<Session> {
    if !bearer.starts_with(TOKEN_PREFIX) {
        return None;
    }
    let digest = hex::encode(Sha256::digest(bearer.as_bytes()));
    let sessions = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    sessions
        .get(&digest)
        .filter(|s| s.expires > Instant::now())
        .cloned()
}

/// Base64 of 32 bytes, or `None`.
#[must_use]
pub fn decode_key(text: &str) -> Option<[u8; 32]> {
    let Ok(bytes) = b64().decode(text.trim()) else {
        return None;
    };
    // Any length but 32 is simply not a key.
    if let Ok(key) = bytes.try_into() {
        Some(key)
    } else {
        None
    }
}

/// The keys listed in `$VIVI_HOME/server/host_keys`, normalized to base64.
fn host_keys() -> Vec<String> {
    let Ok(path) = crate::home::server_home().map(|h| h.join("host_keys")) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter_map(|line| line.split_whitespace().find_map(decode_key))
        .map(|key| b64().encode(key))
        .collect()
}
