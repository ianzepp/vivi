//! The `vivi mcp` stdio bridge engine: newline-delimited JSON-RPC 2.0 in,
//! the project server's answers out, one line each.
//!
//! The bridge is a pass-through. It never inspects `method`: a message with
//! an `id` member is a request and expects an answer, a message without one
//! is a notification and never gets a reply, so new server tools need no
//! bridge change. Every valid line is forwarded byte-for-byte to the
//! project server's MCP endpoint; a line that cannot be served is answered
//! in-protocol (-32700 / -32600 / -32000) and the session continues. Bearer
//! tokens live in memory for one serve and are never written anywhere.
//!
//! The CLI (flags, environment, the `[sync]` fallback) is a later unit;
//! this module is the engine and its tests.

use std::io::{BufRead, Write};

use chrono::Utc;
use serde_json::{Value, json};

use crate::error::VivariumError;
use crate::sync::SyncSettings;
use crate::sync::http::{MintedToken, RawResponse, mint_token, send_raw};
use crate::sync::identity::SyncIdentity;

/// A member token is renewed when it is within this window of expiry.
const EXPIRY_SLACK_SECS: i64 = 30;

/// The status that ends a member request with one renewal and one retry.
const UNAUTHORIZED: u16 = 401;

/// JSON-RPC code for a line that is not JSON.
const CODE_PARSE_ERROR: i64 = -32_700;

/// JSON-RPC code for a JSON value that is no request, notification, or
/// batch.
const CODE_INVALID_REQUEST: i64 = -32_600;

/// JSON-RPC code for a request that could not be served.
const CODE_UNSERVED: i64 = -32_000;

/// How the bridge authenticates to the project server.
#[derive(Debug, Clone)]
pub enum Credential {
    /// A pre-shared API key, sent as the bearer as it is: never minted,
    /// never renewed, and a 401 is final.
    ApiKey(String),
    /// A project member identity: challenge-signed bearers are minted
    /// lazily, kept in memory only, and renewed near expiry or on a 401.
    Member(SyncIdentity),
}

/// Everything one bridge session needs: where the server is and how to
/// authenticate.
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Sync settings; `server`, `project`, and `name` are used, and
    /// `mailspace_id` may be empty.
    pub settings: SyncSettings,
    /// The credential presented as the bearer on every forwarded request.
    pub credential: Credential,
}

/// The upstream the bridge posts to. A trait so the engine is testable
/// without sockets: tests script the answers.
pub trait Upstream {
    /// POST `body` to the project's MCP endpoint with
    /// `Authorization: Bearer <bearer>`.
    ///
    /// # Errors
    /// Returns an error only when the server cannot be reached; every HTTP
    /// status comes back inside the [`RawResponse`].
    fn post(&self, bearer: &str, body: &[u8]) -> Result<RawResponse, VivariumError>;

    /// Mint a fresh member bearer.
    ///
    /// # Errors
    /// Returns an error when the mint is refused or the server cannot be
    /// reached.
    fn mint(&self) -> Result<MintedToken, VivariumError>;
}

/// The real upstream: one blocking [`send_raw`] per forwarded message and
/// [`mint_token`] per bearer.
pub struct HttpUpstream<'a> {
    settings: &'a SyncSettings,
    identity: Option<&'a SyncIdentity>,
}

impl<'a> HttpUpstream<'a> {
    /// Build the upstream for `settings`; `identity` is needed only for a
    /// [`Credential::Member`] mint.
    #[must_use]
    pub fn new(settings: &'a SyncSettings, identity: Option<&'a SyncIdentity>) -> Self {
        Self { settings, identity }
    }
}

impl Upstream for HttpUpstream<'_> {
    fn post(&self, bearer: &str, body: &[u8]) -> Result<RawResponse, VivariumError> {
        send_raw(
            "POST",
            &format!("{}/mcp", mcp_base_url(self.settings)),
            &[
                ("Authorization", format!("Bearer {bearer}")),
                ("Content-Type", "application/json".to_string()),
                ("Accept", "application/json, text/event-stream".to_string()),
            ],
            Some(body.to_vec()),
        )
    }

    fn mint(&self) -> Result<MintedToken, VivariumError> {
        let identity = self.identity.ok_or_else(|| {
            VivariumError::Message(
                "the MCP bridge has no member identity, so it cannot mint a bearer".into(),
            )
        })?;
        mint_token(self.settings, identity)
    }
}

/// The project's MCP endpoint: `{server}/v1/projects/{project}/mcp`.
#[must_use]
fn mcp_base_url(settings: &SyncSettings) -> String {
    format!("{}/v1/projects/{}/mcp", settings.server, settings.project)
}

/// Serve one bridge session: read newline-delimited JSON-RPC 2.0 from
/// `input`, forward every valid line unchanged to the project server's MCP
/// endpoint, write the server's reply as one line (one per SSE message
/// frame) on `output`, and write diagnostics prefixed `vivi mcp: ` to
/// `diag`.
///
/// # Errors
/// Returns an error only when `input` cannot be read or a sink cannot be
/// written; every per-message failure is answered in-protocol and the
/// session continues to the next line.
pub fn serve(
    config: &BridgeConfig,
    input: impl BufRead,
    output: impl Write,
    diag: impl Write,
) -> Result<(), VivariumError> {
    let identity = match &config.credential {
        Credential::Member(identity) => Some(identity),
        Credential::ApiKey(_) => None,
    };
    let upstream = HttpUpstream::new(&config.settings, identity);
    serve_with(&upstream, &config.credential, input, output, diag)
}

/// The engine behind [`serve`], against any [`Upstream`].
///
/// # Errors
/// Same contract as [`serve`].
pub fn serve_with(
    upstream: &dyn Upstream,
    credential: &Credential,
    input: impl BufRead,
    output: impl Write,
    diag: impl Write,
) -> Result<(), VivariumError> {
    let mut session = Session::new(upstream, credential, output, diag);
    for line in input.lines() {
        session.handle_line(&line?)?;
    }
    Ok(())
}

/// One serve pass: the upstream, the credential, the in-memory bearer, and
/// the two sinks.
struct Session<'a, O: Write, D: Write> {
    upstream: &'a dyn Upstream,
    credential: &'a Credential,
    token: Option<MintedToken>,
    output: O,
    diag: D,
}

impl<'a, O: Write, D: Write> Session<'a, O, D> {
    fn new(upstream: &'a dyn Upstream, credential: &'a Credential, output: O, diag: D) -> Self {
        Self {
            upstream,
            credential,
            token: None,
            output,
            diag,
        }
    }

    /// One line: blank lines are skipped, invalid JSON and non-message JSON
    /// are answered in-protocol, and valid messages are forwarded.
    fn handle_line(&mut self, line: &str) -> Result<(), VivariumError> {
        if line.trim().is_empty() {
            return Ok(());
        }
        let message = match serde_json::from_str::<Value>(line) {
            Ok(message) => message,
            Err(error) => {
                return self.write_json(&parse_error(&error.to_string()));
            }
        };
        match Kind::classify(&message) {
            Some(kind) => self.forward(line, kind),
            None => self.write_json(&invalid_request()),
        }
    }

    /// Send one message upstream and answer it: a request gets its reply or
    /// a -32000 error, a notification gets silence (failures go to `diag`),
    /// and a batch is answered once with id null.
    fn forward(&mut self, line: &str, kind: Kind) -> Result<(), VivariumError> {
        match kind {
            Kind::Request { id } => match self.serve(line.as_bytes()) {
                Ok(outcome) => self.write_outcome(&outcome),
                Err(failure) => self.write_json(&request_error(Some(&id), &failure)),
            },
            Kind::Notification => {
                if let Err(failure) = self.serve(line.as_bytes()) {
                    self.diag_line(&failure.message)?;
                }
                Ok(())
            }
            Kind::Batch => match self.serve(line.as_bytes()) {
                Ok(outcome) => self.write_outcome(&outcome),
                Err(failure) => self.write_json(&request_error(None, &failure)),
            },
        }
    }

    /// Forward one message body: bearer, POST, and for a member exactly one
    /// renewal and retry when the server answers 401.
    fn serve(&mut self, body: &[u8]) -> Result<Outcome, Failure> {
        let response = self.post(body)?;
        if response.status == UNAUTHORIZED && self.is_member() {
            self.renew()?;
            return classify_reply(self.post(body)?);
        }
        classify_reply(response)
    }

    /// One POST with the current bearer: a transport failure is a
    /// [`Failure`]; every HTTP status comes back.
    fn post(&mut self, body: &[u8]) -> Result<RawResponse, Failure> {
        let bearer = self.bearer()?;
        self.upstream
            .post(&bearer, body)
            .map_err(|error| Failure::unreachable(&error.to_string()))
    }

    /// The bearer for the next POST: the API key as given, or the member
    /// token, minted on first use and renewed when it is within
    /// [`EXPIRY_SLACK_SECS`] of expiry (or past it).
    fn bearer(&mut self) -> Result<String, Failure> {
        match self.credential {
            Credential::ApiKey(key) => Ok(key.clone()),
            Credential::Member(_) => {
                let stale = self.token.as_ref().is_none_or(|token| !token_fresh(token));
                if stale {
                    self.mint_into()?;
                }
                let Some(token) = self.token.as_ref() else {
                    return Err(Failure::auth("no bearer token was minted"));
                };
                Ok(token.token.clone())
            }
        }
    }

    /// Mint a bearer and keep it in memory only.
    fn mint_into(&mut self) -> Result<(), Failure> {
        let minted = self
            .upstream
            .mint()
            .map_err(|error| Failure::auth(&format!("could not mint a bearer token: {error}")))?;
        self.token = Some(minted);
        Ok(())
    }

    /// Discard the bearer and mint a fresh one (the one 401 retry).
    fn renew(&mut self) -> Result<(), Failure> {
        self.token = None;
        self.mint_into()
    }

    fn is_member(&self) -> bool {
        matches!(self.credential, Credential::Member(_))
    }

    /// Write what a successful forward produced: nothing, or one line per
    /// reply.
    fn write_outcome(&mut self, outcome: &Outcome) -> Result<(), VivariumError> {
        match outcome {
            Outcome::Silent => Ok(()),
            Outcome::Lines(lines) => {
                for line in lines {
                    self.write_line(line)?;
                }
                Ok(())
            }
        }
    }

    /// One protocol message as one flushed line.
    fn write_json(&mut self, message: &Value) -> Result<(), VivariumError> {
        self.write_line(&message.to_string())
    }

    fn write_line(&mut self, line: &str) -> Result<(), VivariumError> {
        writeln!(self.output, "{line}")?;
        self.output.flush()?;
        Ok(())
    }

    fn diag_line(&mut self, text: &str) -> Result<(), VivariumError> {
        writeln!(self.diag, "vivi mcp: {text}")?;
        self.diag.flush()?;
        Ok(())
    }
}

/// How one forwarded message is answered.
enum Kind {
    /// A message with an `id` (any JSON type): expects a reply.
    Request {
        /// The id as it came, verbatim, for the error reply.
        id: Value,
    },
    /// A message with no `id` member (or an `id` of null): never replied to.
    Notification,
    /// A JSON array: answered once, with id null, whatever it holds.
    Batch,
}

impl Kind {
    /// Classify one parsed message; `None` for a JSON value that is neither
    /// object nor array.
    fn classify(message: &Value) -> Option<Self> {
        if let Some(object) = message.as_object() {
            let id = object.get("id").filter(|id| !id.is_null());
            return Some(match id {
                Some(id) => Self::Request { id: id.clone() },
                None => Self::Notification,
            });
        }
        message.is_array().then_some(Self::Batch)
    }
}

/// What one successful forward writes.
enum Outcome {
    /// One line per reply: one for a JSON body, one per SSE message frame.
    Lines(Vec<String>),
    /// An empty answer (a notification's 202/204): write nothing.
    Silent,
}

/// Why one message could not be served: the final text of its error reply
/// and, when the server answered, its status.
struct Failure {
    message: String,
    http_status: Option<u16>,
}

impl Failure {
    /// The server could not be reached.
    fn unreachable(error: &str) -> Self {
        Self {
            message: format!("project server unreachable: {error}"),
            http_status: None,
        }
    }

    /// Authentication ended the request: a mint failed.
    fn auth(reason: &str) -> Self {
        Self {
            message: format!("authentication failed: {reason}"),
            http_status: None,
        }
    }

    /// The server answered a non-2xx: a 401 names authentication, and every
    /// status carries the server's `error.code`/`error.message` when the
    /// body has them.
    fn status(status: u16, body: &[u8]) -> Self {
        let parsed: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        let code = parsed
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let detail = parsed
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("");
        let message = if status == UNAUTHORIZED {
            format!("authentication failed (HTTP {status}, {code}): {detail}")
        } else {
            format!("project server refused the request (HTTP {status}, {code}): {detail}")
        };
        Self {
            message,
            http_status: Some(status),
        }
    }
}

/// Turn one HTTP answer into what to write, or the failure to report. Every
/// non-2xx status is a [`Failure`]; an empty body is silence (what a
/// notification gets from a 202/204).
fn classify_reply(response: RawResponse) -> Result<Outcome, Failure> {
    if !(200..300).contains(&response.status) {
        return Err(Failure::status(response.status, &response.body));
    }
    if response.body.is_empty() {
        return Ok(Outcome::Silent);
    }
    let streamed = response
        .content_type
        .as_deref()
        .is_some_and(|kind| kind.contains("text/event-stream"));
    if streamed {
        return Ok(Outcome::Lines(sse_payloads(&response.body)));
    }
    Ok(Outcome::Lines(vec![one_line(&response.body)]))
}

/// The reply as one output line: a JSON body re-serialized compactly (a
/// pretty or multi-line body becomes one line), any other body trimmed.
#[must_use]
fn one_line(body: &[u8]) -> String {
    match serde_json::from_slice::<Value>(body) {
        Ok(message) => message.to_string(),
        Err(_) => String::from_utf8_lossy(body).trim().to_string(),
    }
}

/// The `data:` payloads of an SSE body, one per `event: message` frame (or
/// a frame with no `event:` line); other events and comments are dropped.
#[must_use]
fn sse_payloads(body: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(body);
    let mut payloads: Vec<String> = Vec::new();
    let mut data: Vec<&str> = Vec::new();
    let mut message_frame = true;
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.is_empty() {
            if message_frame && !data.is_empty() {
                payloads.push(data.join("\n"));
            }
            data.clear();
            message_frame = true;
            continue;
        }
        if let Some(payload) = line.strip_prefix("data:") {
            data.push(payload.strip_prefix(' ').unwrap_or(payload));
        } else if let Some(name) = line.strip_prefix("event:") {
            message_frame = name.trim() == "message";
        }
    }
    if message_frame && !data.is_empty() {
        payloads.push(data.join("\n"));
    }
    payloads
}

/// A member token is fresh unless its `expires_at` parses as RFC 3339 and
/// is within [`EXPIRY_SLACK_SECS`] of now (or past it). An absent or
/// unparsable expiry keeps the token: nothing better is knowable.
fn token_fresh(token: &MintedToken) -> bool {
    let Some(expires_at) = token.expires_at.as_deref() else {
        return true;
    };
    let Ok(at) = chrono::DateTime::parse_from_rfc3339(expires_at) else {
        return true;
    };
    at.timestamp() - Utc::now().timestamp() > EXPIRY_SLACK_SECS
}

/// The -32700 reply for a line that is not JSON.
#[must_use]
fn parse_error(error: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": { "code": CODE_PARSE_ERROR, "message": format!("parse error: {error}") }
    })
}

/// The -32600 reply for a JSON value that is neither object nor array.
#[must_use]
fn invalid_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": {
            "code": CODE_INVALID_REQUEST,
            "message": "invalid request: expected an object or a batch array"
        }
    })
}

/// The -32000 reply for an unserved request (`id` verbatim; null for a
/// batch), with the HTTP status when the server answered.
#[must_use]
fn request_error(id: Option<&Value>, failure: &Failure) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": CODE_UNSERVED,
            "message": failure.message,
            "data": { "http_status": failure.http_status }
        }
    })
}

#[cfg(test)]
#[path = "mcp_test.rs"]
mod tests;
