//! The HTTP side of sync: identity registration, per-invocation bearer mint,
//! append, read, and the member directory against a project server.
//!
//! Tokens live in memory for one invocation and are never written anywhere or
//! printed.

use std::cell::RefCell;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::b64;
use crate::error::VivariumError;
use crate::sync::SyncSettings;
use crate::sync::identity::SyncIdentity;
use crate::sync::transport::{
    AppendFailure, Appended, Envelope, ListQuery, Lookup, Page, Transport,
};

/// Request timeout for project server calls.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Receipt from a successful `sync join`.
#[derive(Debug, Clone)]
pub struct JoinReceipt {
    pub name: String,
    pub fingerprint: String,
    pub token_expires_at: String,
}

#[derive(Debug, Deserialize)]
struct Challenge {
    challenge_id: String,
    challenge: String,
}

#[derive(Debug, Deserialize)]
struct Registered {
    name: String,
    token_expires_at: String,
    fingerprint: String,
}

/// One member of the project directory.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Member {
    pub name: String,
    pub fingerprint: String,
    /// The member's mailspace id; absent for an identity registered before
    /// mailspaces had ids. Such a member has no address prefix.
    #[serde(default)]
    pub mailspace_id: Option<String>,
}

impl Member {
    /// The domain of the member's addresses: `<first 8 of the id>.local`.
    #[must_use]
    pub fn address_domain(&self) -> Option<String> {
        let id = self.mailspace_id.as_deref()?;
        id.get(..8).map(|prefix| format!("{prefix}.local"))
    }
}

impl Member {
    /// `name  <prefix>.local  <16-char fingerprint>`, the domain `-` when the
    /// member has none.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "{}  {}  {}",
            self.name,
            self.address_domain().unwrap_or_else(|| "-".into()),
            self.fingerprint.get(..16).unwrap_or(&self.fingerprint)
        )
    }
}

#[derive(Debug, Deserialize)]
struct Directory {
    #[serde(default)]
    members: Vec<Member>,
}

#[derive(Debug, Deserialize)]
struct Minted {
    token: String,
    #[serde(default)]
    token_expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Accepted {
    message_id: String,
    sequence: u64,
    #[serde(default)]
    duplicate: bool,
    #[serde(default)]
    content_sha256: Option<String>,
}

/// The real [`Transport`]: one bearer per invocation, minted on first use.
pub struct HttpTransport<'a> {
    settings: &'a SyncSettings,
    identity: &'a SyncIdentity,
    token: RefCell<Option<String>>,
}

impl<'a> HttpTransport<'a> {
    #[must_use]
    pub fn new(settings: &'a SyncSettings, identity: &'a SyncIdentity) -> Self {
        Self {
            settings,
            identity,
            token: RefCell::new(None),
        }
    }

    fn bearer(&self) -> Result<String, VivariumError> {
        if let Some(token) = self.token.borrow().as_ref() {
            return Ok(token.clone());
        }
        let token = mint_bearer(self.settings, self.identity)?;
        *self.token.borrow_mut() = Some(token.clone());
        Ok(token)
    }

    /// The project's members, with the per-invocation bearer.
    ///
    /// # Errors
    /// Returns an error when the server cannot be reached, refuses the
    /// bearer, or answers something that is not a directory.
    pub fn directory(&self) -> Result<Vec<Member>, VivariumError> {
        let url = format!("{}/directory", project_base_url(self.settings));
        let (status, body) = request("GET", &url, None, &self.authorized()?)?;
        if status != 200 {
            return Err(api_error(status, &body, self.settings));
        }
        parse_directory(&body)
    }

    fn authorized(&self) -> Result<Headers, VivariumError> {
        Ok(vec![(
            "Authorization",
            format!("Bearer {}", self.bearer()?),
        )])
    }
}

type Headers = Vec<(&'static str, String)>;

impl Transport for HttpTransport<'_> {
    fn append(&self, message_id: &str, envelope: &Envelope<'_>) -> Result<Appended, AppendFailure> {
        let token = self
            .bearer()
            .map_err(|e| AppendFailure::Stop(e.to_string()))?;
        let put = append_request(self.settings, &token, message_id, envelope);
        let (status, body) = send("PUT", &put.url, Body::Raw(put.body), &put.headers)
            .map_err(|e| AppendFailure::Stop(e.to_string()))?;
        classify_append(status, &body)
    }

    fn fetch(&self, query: &ListQuery<'_>) -> Result<Page, VivariumError> {
        let url = list_url(self.settings, query);
        let (status, body) = request("GET", &url, None, &self.authorized()?)?;
        if status != 200 {
            return Err(api_error(status, &body, self.settings));
        }
        parse(&body)
    }

    fn lookup(&self, handles: &[String]) -> Result<Lookup, VivariumError> {
        let url = format!("{}/items/lookup", project_base_url(self.settings));
        let payload = json!({ "handles": handles });
        let (status, body) = request("POST", &url, Some(&payload), &self.authorized()?)?;
        if status != 200 {
            return Err(api_error(status, &body, self.settings));
        }
        parse(&body)
    }
}

/// The list URL: `since` and an explicit `limit` always, `after` when set.
/// The time carries `+` and `:` only in RFC 3339 forms, so it is encoded.
pub(crate) fn list_url(settings: &SyncSettings, query: &ListQuery<'_>) -> String {
    let mut url = format!(
        "{}/items?since={}&limit={}",
        project_base_url(settings),
        query.since,
        query.limit
    );
    if let Some(after) = query.after {
        url.push_str("&after=");
        url.push_str(&encode_query(after));
    }
    url
}

fn encode_query(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(byte).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Read a directory response: `{members: [{name, fingerprint, mailspace_id}]}`.
/// Extra fields are ignored.
pub(crate) fn parse_directory(body: &Value) -> Result<Vec<Member>, VivariumError> {
    let directory: Directory = parse(body)?;
    Ok(directory.members)
}

/// The append request: the letter's exact bytes as a `message/rfc822` body to
/// `PUT .../items/{message_id}`, the envelope in `X-Vivi-*` headers, and no
/// `Message-ID` header (the id is the path segment).
pub(crate) struct AppendRequest {
    pub(crate) url: String,
    pub(crate) headers: Headers,
    pub(crate) body: Vec<u8>,
}

pub(crate) fn append_request(
    settings: &SyncSettings,
    token: &str,
    message_id: &str,
    envelope: &Envelope<'_>,
) -> AppendRequest {
    let mut headers = vec![
        ("Authorization", format!("Bearer {token}")),
        ("Content-Type", "message/rfc822".to_string()),
        ("X-Vivi-Kind", envelope.kind.to_string()),
    ];
    if !envelope.to.is_empty() {
        headers.push(("X-Vivi-To", envelope.to.to_string()));
    }
    AppendRequest {
        url: format!("{}/items/{message_id}", project_base_url(settings)),
        headers,
        body: envelope.raw_eml.to_vec(),
    }
}

/// Interpret one append response.
pub(crate) fn classify_append(status: u16, body: &Value) -> Result<Appended, AppendFailure> {
    let code = error_code(body);
    let detail = error_detail(body);
    match status {
        200 | 201 => {
            let accepted: Accepted = serde_json::from_value(body.clone()).map_err(|_| {
                AppendFailure::Stop("project server returned an unexpected append response".into())
            })?;
            Ok(Appended {
                message_id: accepted.message_id,
                sequence: accepted.sequence,
                duplicate: accepted.duplicate || status == 200,
                content_sha256: accepted.content_sha256,
            })
        }
        400 | 409 | 413 => Err(AppendFailure::Refused(format!(
            "HTTP {status} {code}: {detail}"
        ))),
        429 => Err(AppendFailure::RateLimited(
            "project server rate limit hit (30 sends/min per member); the rest stays unsent".into(),
        )),
        401 => Err(AppendFailure::Stop(
            "project server rejected the bearer; not admitted yet: run vivi sync key, an \
             admitted member runs vivi sync vouch <name> --key-file <pem>, then run vivi sync \
             join"
                .into(),
        )),
        other => Err(AppendFailure::Stop(format!(
            "project server error (HTTP {other}, {code}): {detail}"
        ))),
    }
}

/// Register `[sync] name` with the project: challenge, sign, register. The
/// token in the response is dropped.
///
/// # Errors
/// Returns a friendly error when the server is unreachable, the project is
/// unknown, the key has not been vouched for, or the name is taken.
pub fn join(
    settings: &SyncSettings,
    identity: &SyncIdentity,
) -> Result<JoinReceipt, VivariumError> {
    let base = project_base_url(settings);
    let pem = identity.public_key_pem();
    let challenge = fetch_challenge(settings, identity)?;
    let signature = sign_challenge(identity, &challenge.challenge)?;
    let (status, body) = request(
        "POST",
        &format!("{base}/identities"),
        Some(&json!({
            "name": settings.name,
            "mailspace_id": settings.mailspace_id,
            "public_key_pem": pem,
            "challenge_id": challenge.challenge_id,
            "signature_base64": signature,
        })),
        &[],
    )?;
    if status != 200 && status != 201 {
        return Err(join_error(status, &body, settings, identity));
    }
    let registered: Registered = parse(&body)?;
    Ok(JoinReceipt {
        name: registered.name,
        fingerprint: registered.fingerprint,
        token_expires_at: registered.token_expires_at,
    })
}

#[derive(Debug, Deserialize)]
struct Created {
    project_id: String,
}

/// Create a project on `server` with this identity's public key and
/// `creator_name` as the creator. No authorization header: the creator is
/// not a member yet. Returns the server's new project id.
///
/// # Errors
/// Returns an error when the server cannot be reached or answers anything
/// but 200 or 201, naming the status and the server's code/detail.
pub fn create_project(
    server: &str,
    identity: &SyncIdentity,
    creator_name: &str,
) -> Result<String, VivariumError> {
    let base = server.trim_end_matches('/');
    let (status, body) = request(
        "POST",
        &format!("{base}/v1/projects"),
        Some(&json!({
            "public_key_pem": identity.public_key_pem(),
            "creator_name": creator_name,
        })),
        &[],
    )?;
    if status != 200 && status != 201 {
        return Err(VivariumError::Message(format!(
            "project server refused to create the project (HTTP {status}, {}): {}",
            error_code(&body),
            error_detail(&body)
        )));
    }
    let created: Created = parse(&body)?;
    Ok(created.project_id)
}

/// A minted bearer and, when the server sends one, when it expires.
#[derive(Debug, Clone)]
pub struct MintedToken {
    pub token: String,
    pub expires_at: Option<String>,
}

/// Mint a bearer for `[sync] name`: challenge, sign, `auth/token`.
///
/// Only `settings.server`, `settings.project`, and `settings.name` are used;
/// `mailspace_id` is not, so callers outside a mailspace pass an empty
/// string. The token lives in memory for one invocation and is never written
/// anywhere or printed.
///
/// # Errors
/// Returns a friendly error when the server is unreachable, the name is not
/// registered under this checkout's key (HTTP 401), or the mint is refused.
pub fn mint_token(
    settings: &SyncSettings,
    identity: &SyncIdentity,
) -> Result<MintedToken, VivariumError> {
    let challenge = fetch_challenge(settings, identity)?;
    let signature = sign_challenge(identity, &challenge.challenge)?;
    let (status, body) = request(
        "POST",
        &format!("{}/auth/token", project_base_url(settings)),
        Some(&json!({
            "name": settings.name,
            "challenge_id": challenge.challenge_id,
            "signature_base64": signature,
        })),
        &[],
    )?;
    if status == 401 {
        return Err(VivariumError::Message(format!(
            "project server refused the bearer mint for '{}'; this checkout's key is not \
             registered under that name (run vivi sync key, an admitted member runs vivi sync \
             vouch {} --key-file <pem>, then run vivi sync join; if this checkout's key was \
             lost or replaced, the name stays bound to the old key and the server has no \
             re-key: only the project creator can help)",
            settings.name, settings.name
        )));
    }
    if status != 200 {
        return Err(api_error(status, &body, settings));
    }
    let minted: Minted = parse(&body)?;
    Ok(MintedToken {
        token: minted.token,
        expires_at: minted.token_expires_at,
    })
}

/// The bearer string only, for callers that do not care about expiry.
fn mint_bearer(settings: &SyncSettings, identity: &SyncIdentity) -> Result<String, VivariumError> {
    Ok(mint_token(settings, identity)?.token)
}

fn fetch_challenge(
    settings: &SyncSettings,
    identity: &SyncIdentity,
) -> Result<Challenge, VivariumError> {
    let (status, body) = request(
        "POST",
        &format!("{}/identities/challenge", project_base_url(settings)),
        Some(&json!({ "public_key_pem": identity.public_key_pem() })),
        &[],
    )?;
    if status != 200 {
        return Err(api_error(status, &body, settings));
    }
    parse(&body)
}

fn project_base_url(settings: &SyncSettings) -> String {
    format!("{}/v1/projects/{}", settings.server, settings.project)
}

/// What a request carries: nothing, a JSON document, or raw bytes sent as-is
/// (the caller sets `Content-Type`).
enum Body {
    Empty,
    Json(Value),
    Raw(Vec<u8>),
}

/// The answer to one raw request: whatever status and body came back, an
/// empty body included.
#[derive(Debug, Clone)]
pub struct RawResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// One blocking request of the exact bytes given (the caller sets
/// `Content-Type`), answered with any status and any body: an empty body is
/// a fine answer. Same [`TIMEOUT`] as the JSON calls.
///
/// # Errors
/// Returns an error when the server cannot be reached or times out; every
/// HTTP status is returned as-is inside [`RawResponse`].
pub fn send_raw(
    method: &str,
    url: &str,
    headers: &[(&'static str, String)],
    body: Option<Vec<u8>>,
) -> Result<RawResponse, VivariumError> {
    let payload = body.map_or(Body::Empty, Body::Raw);
    let (status, content_type, body) = send_bytes(method, url, payload, headers)?;
    Ok(RawResponse {
        status,
        content_type,
        body,
    })
}

/// One blocking JSON request, with a JSON body when `payload` is given.
fn request(
    method: &str,
    url: &str,
    payload: Option<&Value>,
    headers: &[(&'static str, String)],
) -> Result<(u16, Value), VivariumError> {
    let body = payload.map_or(Body::Empty, |payload| Body::Json(payload.clone()));
    send(method, url, body, headers)
}

/// One blocking request with a JSON response: status plus parsed body. An
/// empty or non-JSON body is an error.
fn send(
    method: &str,
    url: &str,
    payload: Body,
    headers: &[(&'static str, String)],
) -> Result<(u16, Value), VivariumError> {
    let (status, _content_type, raw) = send_bytes(method, url, payload, headers)?;
    let body: Value = serde_json::from_slice(&raw)
        .map_err(|e| VivariumError::Message(format!("invalid project server response: {e}")))?;
    Ok((status, body))
}

/// Why one request never got its response: a timeout or an unreachable
/// server, each with its own message.
fn transport_error(e: &reqwest::Error) -> VivariumError {
    if e.is_timeout() {
        VivariumError::Message(format!(
            "project server timed out after {}ms",
            TIMEOUT.as_millis()
        ))
    } else {
        VivariumError::Message(format!("project server unreachable: {e}"))
    }
}

/// The one blocking-request plumbing: its own thread and runtime so callers
/// stay synchronous, answered with the status, the response Content-Type,
/// and the raw body.
fn send_bytes(
    method: &str,
    url: &str,
    payload: Body,
    headers: &[(&'static str, String)],
) -> Result<(u16, Option<String>, Vec<u8>), VivariumError> {
    let url = url.to_string();
    let method = method.to_string();
    let headers = headers.to_vec();
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
            let method = reqwest::Method::from_bytes(method.as_bytes())
                .map_err(|e| VivariumError::Other(format!("invalid HTTP method: {e}")))?;
            let mut request = client.request(method, &url);
            for (name, value) in &headers {
                request = request.header(*name, value.as_str());
            }
            request = match payload {
                Body::Empty => request,
                Body::Json(payload) => request.json(&payload),
                Body::Raw(bytes) => request.body(bytes),
            };
            let response = request.send().await.map_err(|e| transport_error(&e))?;
            let status = response.status().as_u16();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().map_or(None, |text| Some(text.to_string())));
            let body = response.bytes().await.map_err(|e| {
                VivariumError::Message(format!("invalid project server response: {e}"))
            })?;
            Ok((status, content_type, body.to_vec()))
        })
    });
    handle
        .join()
        .map_err(|_| VivariumError::Message("sync client thread panicked".into()))?
}

/// Decode and sign an issued challenge (32 bytes), returning base64.
fn sign_challenge(identity: &SyncIdentity, challenge_b64: &str) -> Result<String, VivariumError> {
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

fn join_error(
    status: u16,
    body: &Value,
    settings: &SyncSettings,
    identity: &SyncIdentity,
) -> VivariumError {
    let code = error_code(body);
    match (status, code.as_str()) {
        (409, "admission_required") => VivariumError::Message(format!(
            "this key has not been vouched for as '{}' in project {}.\n  \
             1. the new member runs: vivi sync key   (prints the public-key PEM; send it, \
             never the seed file, to an admitted member)\n  \
             2. an admitted member runs: vivi sync vouch {} --key-file <pem-file>   \
             (fingerprint {})\n\
             then run vivi sync join again. If that name was admitted before with a different \
             key (this checkout's key was lost or replaced), the server binds a name to the \
             first key admitted and has no way to re-key it: the vouch will be refused, and \
             only the project creator can help",
            settings.name,
            settings.project,
            settings.name,
            identity.wire_fingerprint()
        )),
        (409, "conflict") => VivariumError::Message(format!(
            "the name '{}' is already registered in project {}; if this checkout registered it, \
             vivi sync already works; otherwise choose another [sync] name",
            settings.name, settings.project
        )),
        (409, "mailspace_prefix_taken") => VivariumError::Message(format!(
            "another member of project {} already holds the address prefix {} (the first 8 \
             characters of this mailspace's id). If nothing was sent from this mailspace yet, \
             remove both the id line in .vivi/mailspace.toml and the file .vivi/mailspace.id (the \
             copy that restores the id) so a new id is minted, then run vivi sync join again. \
             Letters sent from this mailspace after that carry the new prefix; letters already \
             stored keep the old one",
            settings.project,
            settings.mailspace_id.get(..8).unwrap_or_default()
        )),
        _ => api_error(status, body, settings),
    }
}

fn api_error(status: u16, body: &Value, settings: &SyncSettings) -> VivariumError {
    let code = error_code(body);
    match (status, code.as_str()) {
        (404, _) => VivariumError::Message(format!(
            "project '{}' was not found on {}; vivi sync create makes a new project and \
             prints its id: put the printed id in [sync] project, then run vivi sync join",
            settings.project, settings.server
        )),
        (401, _) => VivariumError::Message(format!(
            "project server rejected the request (unauthorized); if '{}' has not registered \
             yet: run vivi sync key, an admitted member runs vivi sync vouch {} --key-file \
             <pem>, then run vivi sync join",
            settings.name, settings.name
        )),
        _ => VivariumError::Message(format!(
            "project server error (HTTP {status}, {code}): {}",
            error_detail(body)
        )),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(body: &Value) -> Result<T, VivariumError> {
    serde_json::from_value(body.clone()).map_err(|_| {
        VivariumError::Message("project server returned an unexpected response".into())
    })
}

fn error_code(body: &Value) -> String {
    body.pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn error_detail(body: &Value) -> String {
    body.pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
#[path = "http_test.rs"]
mod tests;

#[cfg(test)]
#[path = "refusal_messages_test.rs"]
mod refusal_messages;
