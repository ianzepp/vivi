//! The `vivi mcp` stdio bridge engine: newline-delimited JSON-RPC 2.0 in,
//! the project server's answers out, one line each.
//!
//! The bridge is a pass-through. A message with an `id` member is a request
//! and expects an answer, a message without one is a notification and never
//! gets a reply, so new server tools need no bridge change. The one place it
//! inspects `method` is a `tools/call`: with a configured project, a call of
//! a tool that takes `project` and carries none gets it filled in (see
//! [`inject`]). Every other valid line is forwarded byte-for-byte to the
//! server's MCP endpoint; a line that cannot be served is answered
//! in-protocol (-32700 / -32600 / -32000) and the session continues. The API
//! key is held in memory and never written.
//!
//! The CLI (flags, environment, the mailspace fallback) lives in
//! [`resolve`] and the binary's command handlers; this module is the engine
//! and its tests.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

use crate::error::VivariumError;
use crate::sync::http::{RawResponse, send_raw, server_mcp_url};

pub mod client;
mod inject;
pub mod resolve;

use inject::Injector;

/// The status that names authentication in a failure message.
const UNAUTHORIZED: u16 = 401;

/// JSON-RPC code for a line that is not JSON.
const CODE_PARSE_ERROR: i64 = -32_700;

/// JSON-RPC code for a JSON value that is no request, notification, or
/// batch.
const CODE_INVALID_REQUEST: i64 = -32_600;

/// JSON-RPC code for a request that could not be served.
const CODE_UNSERVED: i64 = -32_000;

/// Everything one bridge session needs: where the server is and the API key
/// presented as the bearer on every forwarded request.
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// The project server's base URL.
    pub server: String,
    /// The API key, sent as the bearer as it is.
    pub api_key: String,
    /// The project the bridge fills into tool calls that take one and
    /// carry none; `None` is a pure pass-through.
    pub project: Option<String>,
}

/// The upstream the bridge posts to. A trait so the engine is testable
/// without sockets: tests script the answers.
pub trait Upstream {
    /// POST `body` to the server's MCP endpoint with
    /// `Authorization: Bearer <bearer>`.
    ///
    /// # Errors
    /// Returns an error only when the server cannot be reached; every HTTP
    /// status comes back inside the [`RawResponse`].
    fn post(&self, bearer: &str, body: &[u8]) -> Result<RawResponse, VivariumError>;
}

/// The real upstream: one blocking [`send_raw`] per forwarded message.
pub struct HttpUpstream<'a> {
    server: &'a str,
}

impl<'a> HttpUpstream<'a> {
    /// Build the upstream for the project server at `server`.
    #[must_use]
    pub fn new(server: &'a str) -> Self {
        Self { server }
    }
}

impl Upstream for HttpUpstream<'_> {
    fn post(&self, bearer: &str, body: &[u8]) -> Result<RawResponse, VivariumError> {
        send_raw(
            "POST",
            &server_mcp_url(self.server),
            &[
                ("Authorization", format!("Bearer {bearer}")),
                ("Content-Type", "application/json".to_string()),
                ("Accept", "application/json, text/event-stream".to_string()),
            ],
            Some(body.to_vec()),
        )
    }
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
    let upstream = HttpUpstream::new(&config.server);
    serve_with_project(
        &upstream,
        &config.api_key,
        config.project.as_deref(),
        input,
        output,
        diag,
    )
}

/// The engine behind [`serve`], against any [`Upstream`].
///
/// # Errors
/// Same contract as [`serve`].
pub fn serve_with(
    upstream: &dyn Upstream,
    api_key: &str,
    input: impl BufRead,
    output: impl Write,
    diag: impl Write,
) -> Result<(), VivariumError> {
    serve_with_project(upstream, api_key, None, input, output, diag)
}

/// [`serve_with`] that fills `project` into tool calls that take one and
/// carry none.
///
/// # Errors
/// Same contract as [`serve`].
pub fn serve_with_project(
    upstream: &dyn Upstream,
    api_key: &str,
    project: Option<&str>,
    input: impl BufRead,
    output: impl Write,
    diag: impl Write,
) -> Result<(), VivariumError> {
    let mut session = Session::new(upstream, api_key, output, diag).with_project(project);
    for line in input.lines() {
        session.handle_line(&line?)?;
    }
    Ok(())
}

/// One serve pass: the upstream, the API key, and the two sinks.
struct Session<'a, O: Write, D: Write> {
    upstream: &'a dyn Upstream,
    api_key: &'a str,
    output: O,
    diag: D,
    injector: Injector,
}

impl<'a, O: Write, D: Write> Session<'a, O, D> {
    fn new(upstream: &'a dyn Upstream, api_key: &'a str, output: O, diag: D) -> Self {
        Self {
            upstream,
            api_key,
            output,
            diag,
            injector: Injector::new(None),
        }
    }

    /// Fill `project` into tool calls that take one and carry none.
    fn with_project(mut self, project: Option<&str>) -> Self {
        self.injector = Injector::new(project);
        self
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
        let Some(kind) = Kind::classify(&message) else {
            return self.write_json(&invalid_request());
        };
        let sent = self
            .injector
            .apply(self.upstream, self.api_key, line, &message);
        if let Some(text) = &sent.diag {
            self.diag_line(text)?;
        }
        self.forward(&sent.body, kind)
    }

    /// Send one message upstream and answer it: a request gets its reply or
    /// a -32000 error, a notification gets silence (failures go to `diag`),
    /// and a batch is answered once with id null.
    fn forward(&mut self, body: &str, kind: Kind) -> Result<(), VivariumError> {
        match kind {
            Kind::Request { id } => match self.serve(body.as_bytes()) {
                Ok(outcome) => self.write_outcome(&outcome),
                Err(failure) => self.write_json(&request_error(Some(&id), &failure)),
            },
            Kind::Notification => {
                if let Err(failure) = self.serve(body.as_bytes()) {
                    self.diag_line(&failure.message)?;
                }
                Ok(())
            }
            Kind::Batch => match self.serve(body.as_bytes()) {
                Ok(outcome) => self.write_outcome(&outcome),
                Err(failure) => self.write_json(&request_error(None, &failure)),
            },
        }
    }

    /// Forward one message body: bearer, POST, and the reply classified.
    fn serve(&mut self, body: &[u8]) -> Result<Outcome, Failure> {
        classify_reply(self.post(body)?)
    }

    /// One POST with the API key as the bearer: a transport failure is a
    /// [`Failure`]; every HTTP status comes back.
    fn post(&mut self, body: &[u8]) -> Result<RawResponse, Failure> {
        self.upstream
            .post(self.api_key, body)
            .map_err(|error| Failure::unreachable(&error.to_string()))
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
