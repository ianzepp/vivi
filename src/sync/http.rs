//! The HTTP side of sync: identity registration, per-invocation bearer mint,
//! append, read, and the member directory against a project server.
//!
//! The blocking request plumbing is the chat client's; only its error text is
//! rebranded. Tokens live in memory for one invocation and are never written
//! anywhere or printed.

use std::cell::RefCell;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::chat::client::{Body, request_json_with_timeout, request_with_body, sign_challenge};
use crate::chat::identity::ChatIdentity;
use crate::error::VivariumError;
use crate::sync::SyncSettings;
use crate::sync::transport::{AppendFailure, Appended, Envelope, Page, Transport};

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
    identity: &'a ChatIdentity,
    token: RefCell<Option<String>>,
}

impl<'a> HttpTransport<'a> {
    #[must_use]
    pub fn new(settings: &'a SyncSettings, identity: &'a ChatIdentity) -> Self {
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
        let url = format!("{}/directory", project_url(self.settings));
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
        let (status, body) =
            request_with_body("PUT", &put.url, Body::Raw(put.body), &put.headers, TIMEOUT)
                .map_err(|e| AppendFailure::Stop(rebrand(e).to_string()))?;
        classify_append(status, &body)
    }

    fn fetch(&self, since: u64) -> Result<Page, VivariumError> {
        let url = format!("{}/items?since={since}", project_url(self.settings));
        let (status, body) = request("GET", &url, None, &self.authorized()?)?;
        if status != 200 {
            return Err(api_error(status, &body, self.settings));
        }
        parse(&body)
    }
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
        url: format!("{}/items/{message_id}", project_url(settings)),
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
        429 => Err(AppendFailure::Stop(
            "project server rate limit hit (30 sends/min per member); the rest stays unsent".into(),
        )),
        401 => Err(AppendFailure::Stop(
            "project server rejected the bearer; run vivi sync join if this identity has not \
             registered"
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
    identity: &ChatIdentity,
) -> Result<JoinReceipt, VivariumError> {
    let base = project_url(settings);
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

/// Mint a bearer for `[sync] name`: challenge, sign, `auth/token`.
fn mint_bearer(settings: &SyncSettings, identity: &ChatIdentity) -> Result<String, VivariumError> {
    let challenge = fetch_challenge(settings, identity)?;
    let signature = sign_challenge(identity, &challenge.challenge)?;
    let (status, body) = request(
        "POST",
        &format!("{}/auth/token", project_url(settings)),
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
             registered under that name (run vivi sync join)",
            settings.name
        )));
    }
    if status != 200 {
        return Err(api_error(status, &body, settings));
    }
    let minted: Minted = parse(&body)?;
    Ok(minted.token)
}

fn fetch_challenge(
    settings: &SyncSettings,
    identity: &ChatIdentity,
) -> Result<Challenge, VivariumError> {
    let (status, body) = request(
        "POST",
        &format!("{}/identities/challenge", project_url(settings)),
        Some(&json!({ "public_key_pem": identity.public_key_pem() })),
        &[],
    )?;
    if status != 200 {
        return Err(api_error(status, &body, settings));
    }
    parse(&body)
}

fn project_url(settings: &SyncSettings) -> String {
    format!("{}/v1/projects/{}", settings.server, settings.project)
}

/// One blocking request through the chat client's plumbing, with its error
/// text rebranded from "chat server" to "project server".
fn request(
    method: &str,
    url: &str,
    payload: Option<&Value>,
    headers: &[(&'static str, String)],
) -> Result<(u16, Value), VivariumError> {
    request_json_with_timeout(method, url, payload, headers, TIMEOUT).map_err(rebrand)
}

pub(crate) fn rebrand(error: VivariumError) -> VivariumError {
    match error {
        VivariumError::Message(text) => {
            VivariumError::Message(text.replacen("chat server", "project server", 1))
        }
        other => other,
    }
}

fn join_error(
    status: u16,
    body: &Value,
    settings: &SyncSettings,
    identity: &ChatIdentity,
) -> VivariumError {
    let code = error_code(body);
    match (status, code.as_str()) {
        (409, "admission_required") => VivariumError::Message(format!(
            "this key has not been vouched for as '{}' in project {}; ask a member to vouch for \
             fingerprint {} under that name, then run vivi sync join again",
            settings.name,
            settings.project,
            identity.wire_fingerprint()
        )),
        (409, "conflict") => VivariumError::Message(format!(
            "the name '{}' is already registered in project {}; if this checkout registered it, \
             vivi sync already works; otherwise choose another [sync] name",
            settings.name, settings.project
        )),
        (409, "mailspace_prefix_taken") => VivariumError::Message(format!(
            "another member of project {} already holds the address prefix {} (the first 8 \
             characters of this mailspace's id); if nothing was sent from this mailspace yet, \
             remove the id line from .vivi/mailspace.toml so a new one is minted, then run \
             vivi sync join again",
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
            "project '{}' was not found on {}",
            settings.project, settings.server
        )),
        (401, _) => VivariumError::Message(
            "project server rejected the request (unauthorized); run vivi sync join if this \
             identity has not registered"
                .into(),
        ),
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
