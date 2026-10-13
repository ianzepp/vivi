#![allow(dead_code)]
//! An HTTP fake of the project server, served in-process on a loopback port.
//!
//! It speaks the real wire contract — the identity challenge/token dance,
//! JSON-RPC 2.0 `/mcp`, and API-key mint/list/revoke — so tests (including
//! child processes such as `vivi mcp` or `vivi sync`) can point at a server
//! this process controls. Include it from a test target with
//! `#[path = "support/project_server.rs"] mod project_server;`; the
//! `#![allow(dead_code)]` above covers the subset one target does not use.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{Value, json};

use vivi::b64;

/// The one project id this fake serves.
const PROJECT: &str = "abcde-fghjk";
/// Default `/auth/token` lifetime, reflected in `token_expires_at`.
const DEFAULT_TOKEN_LIFETIME_SECS: i64 = 3600;
/// `/keys` refuses a new mint once this many live keys exist.
const MAX_LIVE_KEYS: usize = 32;
/// MCP protocol versions the fake echoes back verbatim.
const SUPPORTED_PROTOCOLS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-11-25"];
/// MCP protocol version handed back when the client's is unknown.
const FALLBACK_PROTOCOL: &str = "2025-03-26";
/// Per-read socket timeout so a stuck client cannot wedge a thread forever.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Idle sleep of the nonblocking accept loop while it watches the stop flag.
const ACCEPT_IDLE: Duration = Duration::from_millis(5);

/// One `/mcp` request as the fake saw it, recorded before any judgment.
#[derive(Clone)]
pub struct McpSeen {
    pub bearer: String,
    pub body: Value,
    pub accept: Option<String>,
}

/// One API key the fake minted. `secret` is the raw key, visible only here.
#[derive(Clone)]
pub struct KeyRecord {
    pub key_id: String,
    pub label: String,
    pub created_by: String,
    pub secret: String,
    pub revoked: bool,
}

/// One loopback HTTP reply.
#[derive(Clone)]
pub struct HttpReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// One reply appended to a seeded item: who said it and what.
#[derive(Clone)]
pub struct ReplyRecord {
    pub by: String,
    pub body: String,
}

/// One seeded item: the stand-in of a task the fake serves through its
/// `show` and `thread` tools.
#[derive(Clone)]
pub struct Item {
    pub handle: String,
    pub subject: String,
    pub body: String,
    pub state: String,
    pub replies: Vec<ReplyRecord>,
}

/// One report the fake recorded: a `reply` or a `done` tool call, with the
/// member that made it (`kind` is `"reply"` or `"done"`; a `done` carries
/// no body).
#[derive(Clone)]
pub struct Report {
    pub kind: &'static str,
    pub item: String,
    pub by: String,
    pub body: String,
}

/// A registered member: the name is bound to the first PEM admitted.
struct Member {
    name: String,
    pem: String,
}

/// A challenge issued against one PEM, single use.
struct Challenge {
    id: String,
    pem: String,
    bytes: Vec<u8>,
    used: bool,
}

/// A minted bearer token.
struct Token {
    secret: String,
    member: String,
    revoked: bool,
}

/// A minted API key, with the stamps the list route reports. Revoked keys
/// stay in the list with a `revoked_at`; live ones have none.
struct StoredKey {
    key_id: String,
    label: String,
    created_by: String,
    secret: String,
    revoked: bool,
    revoked_at: Option<String>,
    created_at: String,
}

/// All mutable server state, behind one mutex.
struct State {
    members: Vec<Member>,
    challenges: Vec<Challenge>,
    tokens: Vec<Token>,
    keys: Vec<StoredKey>,
    mcp_seen: Vec<McpSeen>,
    items: Vec<Item>,
    reports: Vec<Report>,
    refuse_next: usize,
    refuse_always: bool,
    token_lifetime_secs: i64,
    tokens_minted: usize,
    requests_seen: usize,
    seq: u64,
    rng: u64,
}

impl State {
    fn new() -> State {
        State {
            members: Vec::new(),
            challenges: Vec::new(),
            tokens: Vec::new(),
            keys: Vec::new(),
            mcp_seen: Vec::new(),
            items: Vec::new(),
            reports: Vec::new(),
            refuse_next: 0,
            refuse_always: false,
            token_lifetime_secs: DEFAULT_TOKEN_LIFETIME_SECS,
            tokens_minted: 0,
            requests_seen: 0,
            seq: 0,
            rng: now_nanos() | 1,
        }
    }
}

/// The running fake: serves until dropped.
pub struct ProjectServer {
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    port: u16,
    accepter: Option<std::thread::JoinHandle<()>>,
}

impl ProjectServer {
    /// Bind 127.0.0.1:0 and serve on background threads until dropped.
    #[must_use]
    pub fn start() -> ProjectServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let port = listener.local_addr().expect("bound address").port();
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");
        let state = Arc::new(Mutex::new(State::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let accepter = accept_loop(listener, Arc::clone(&state), Arc::clone(&stop));
        ProjectServer {
            state,
            stop,
            port,
            accepter: Some(accepter),
        }
    }

    /// The server root, `http://127.0.0.1:<port>`.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The one project id this fake serves.
    #[must_use]
    pub fn project(&self) -> &'static str {
        PROJECT
    }

    /// Admit a member directly: `name` is bound to `public_key_pem`.
    pub fn register(&self, name: &str, public_key_pem: &str) {
        self.state.lock().unwrap().members.push(Member {
            name: name.to_string(),
            pem: public_key_pem.to_string(),
        });
    }

    /// Refuse the next `n` `/mcp` requests with 401 whatever the bearer.
    pub fn refuse_mcp_next(&self, n: usize) {
        self.state.lock().unwrap().refuse_next = n;
    }

    /// Refuse every further `/mcp` request with 401 whatever the bearer.
    pub fn refuse_mcp_always(&self) {
        self.state.lock().unwrap().refuse_always = true;
    }

    /// Change the lifetime reported for tokens minted from now on.
    pub fn set_token_lifetime_secs(&self, secs: i64) {
        self.state.lock().unwrap().token_lifetime_secs = secs;
    }

    /// Make every minted token stop working.
    pub fn revoke_all_tokens(&self) {
        for token in &mut self.state.lock().unwrap().tokens {
            token.revoked = true;
        }
    }

    /// How many tokens this fake has minted.
    #[must_use]
    pub fn tokens_minted(&self) -> usize {
        self.state.lock().unwrap().tokens_minted
    }

    /// How many HTTP requests this fake has answered, whatever the route or
    /// the answer: the counter a `before any request` claim is checked with.
    #[must_use]
    pub fn requests_seen(&self) -> usize {
        self.state.lock().unwrap().requests_seen
    }

    /// Mint an API key for `member` directly, skipping the token dance, so
    /// a test can hand `VIVI_MCP_API_KEY` to a child without minting a
    /// bearer token first. Returns the secret.
    pub fn issue_api_key(&self, member: &str) -> String {
        let mut state = self.state.lock().unwrap();
        state.seq += 1;
        let key_id = format!("key_{}", state.seq);
        let secret = format!("vivi_{}", hex_chars(&mut state.rng, 32));
        state.keys.push(StoredKey {
            key_id,
            label: "test".to_string(),
            created_by: member.to_string(),
            secret: secret.clone(),
            revoked: false,
            revoked_at: None,
            created_at: rfc3339(now_secs()),
        });
        secret
    }

    /// Every `/mcp` request so far, in arrival order.
    #[must_use]
    pub fn mcp_requests(&self) -> Vec<McpSeen> {
        self.state.lock().unwrap().mcp_seen.clone()
    }

    /// Seed one item the `show` and `thread` tools serve. Seeding the same
    /// handle again replaces the item.
    pub fn seed_item(&self, handle: &str, subject: &str, body: &str) {
        let mut state = self.state.lock().unwrap();
        let item = Item {
            handle: handle.to_string(),
            subject: subject.to_string(),
            body: body.to_string(),
            state: "open".to_string(),
            replies: Vec::new(),
        };
        match state.items.iter().position(|one| one.handle == handle) {
            Some(index) => state.items[index] = item,
            None => state.items.push(item),
        }
    }

    /// Every `reply` and `done` the fake recorded, in arrival order, with
    /// the member that made each.
    #[must_use]
    pub fn reports(&self) -> Vec<Report> {
        self.state.lock().unwrap().reports.clone()
    }

    /// Every API key this fake minted, with secrets.
    #[must_use]
    pub fn keys(&self) -> Vec<KeyRecord> {
        self.state
            .lock()
            .unwrap()
            .keys
            .iter()
            .map(|key| KeyRecord {
                key_id: key.key_id.clone(),
                label: key.label.clone(),
                created_by: key.created_by.clone(),
                secret: key.secret.clone(),
                revoked: key.revoked,
            })
            .collect()
    }
}

impl Drop for ProjectServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(accepter) = self.accepter.take() {
            let _ = accepter.join();
        }
    }
}

/// Accept connections until the stop flag is set; one thread per connection.
fn accept_loop(
    listener: TcpListener,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let state = Arc::clone(&state);
                    std::thread::spawn(move || serve_connection(state, stream));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(ACCEPT_IDLE);
                }
                Err(_) => break,
            }
        }
    })
}

/// Answer one keep-alive-free HTTP/1.1 request, then close.
fn serve_connection(state: Arc<Mutex<State>>, mut stream: TcpStream) {
    // BSD/macOS hand `O_NONBLOCK` down from the nonblocking listener to
    // every accepted socket; a read on such a stream returns `WouldBlock`
    // while the request bytes are still in flight, which `read_request`
    // treats as a closed connection, so the fake would close without
    // replying. Clear the flag so reads block, bounded by `READ_TIMEOUT`,
    // on every platform (Linux already accepts blocking sockets).
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    if stream.set_read_timeout(Some(READ_TIMEOUT)).is_err() {
        return;
    }
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    state.lock().unwrap().requests_seen += 1;
    let reply = route(&state, &request);
    let _ = write_response(&mut stream, &reply);
}

/// An HTTP/1.1 request: request line, lowercased headers, Content-Length body.
struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The bearer token value, scheme stripped.
    fn bearer(&self) -> Option<&str> {
        let value = self.header("authorization")?;
        value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
    }
}

/// Read one request off the socket: head, then a Content-Length body.
fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(end) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        raw.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let parsed = parse_head(&head)?;
    let length = parsed
        .header("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = raw[head_end..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    Some(Request { body, ..parsed })
}

/// Parse the request line and headers; the body is filled in by the caller.
fn parse_head(head: &str) -> Option<Request> {
    let mut lines = head.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    Some(Request {
        method,
        path,
        headers,
        body: Vec::new(),
    })
}

/// An internal reply: status, content type (`""` for none) and a text body.
struct Reply {
    status: u16,
    content_type: &'static str,
    body: String,
}

/// A JSON reply.
fn json_reply(status: u16, value: Value) -> Reply {
    Reply {
        status,
        content_type: "application/json",
        body: value.to_string(),
    }
}

/// The one error shape: `{"error":{"code":...,"message":...}}`.
fn error_reply(status: u16, code: &str, message: &str) -> Reply {
    json_reply(
        status,
        json!({ "error": { "code": code, "message": message } }),
    )
}

/// A status-only reply with an empty body (202, 204).
fn empty_reply(status: u16) -> Reply {
    Reply {
        status,
        content_type: "",
        body: String::new(),
    }
}

/// Write a reply with `Connection: close`; 204 carries no body.
fn write_response(stream: &mut TcpStream, reply: &Reply) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        reply.status,
        reason_phrase(reply.status)
    );
    if !reply.content_type.is_empty() {
        head.push_str(&format!("Content-Type: {}\r\n", reply.content_type));
    }
    head.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    if reply.status != 204 {
        stream.write_all(reply.body.as_bytes())?;
    }
    stream.flush()
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Reply",
    }
}

/// Route under `/v1/projects/{project}`, in the contract's priority order.
fn route(state: &Mutex<State>, request: &Request) -> Reply {
    let Some(rest) = request.path.strip_prefix("/v1/projects/") else {
        return error_reply(404, "not_found", "paths live under /v1/projects/{project}");
    };
    let Some((project, tail)) = rest.split_once('/') else {
        return error_reply(404, "not_found", "no route sits directly under the project");
    };
    if project != PROJECT {
        return error_reply(
            404,
            "project_not_found",
            "this fake serves one project only",
        );
    }
    if request.method == "DELETE"
        && let Some(key_id) = tail.strip_prefix("keys/")
    {
        return revoke_key(state, request, key_id);
    }
    match (request.method.as_str(), tail) {
        ("POST", "identities/challenge") => issue_challenge(state, &request.body),
        ("POST", "auth/token") => mint_token(state, request),
        ("POST", "mcp") => handle_mcp(state, request),
        ("POST", "keys") => create_key(state, request),
        ("GET", "keys") => list_keys(state, request),
        _ => error_reply(404, "not_found", "no such route on the fake project server"),
    }
}

/// `POST /identities/challenge`: 32 distinct bytes bound to the given PEM.
fn issue_challenge(state: &Mutex<State>, body: &[u8]) -> Reply {
    let Ok(parsed) = serde_json::from_slice::<Value>(body) else {
        return error_reply(400, "invalid_request", "body must be a JSON object");
    };
    let Some(pem) = parsed.get("public_key_pem").and_then(Value::as_str) else {
        return error_reply(400, "invalid_request", "public_key_pem is required");
    };
    let mut state = state.lock().unwrap();
    state.seq += 1;
    let id = format!("challenge-{}", state.seq);
    let bytes = distinct_bytes(&mut state.rng).to_vec();
    state.challenges.push(Challenge {
        id: id.clone(),
        pem: pem.to_string(),
        bytes: bytes.clone(),
        used: false,
    });
    json_reply(
        200,
        json!({ "challenge_id": id, "challenge": b64::encode(&bytes), "expires_in": 60 }),
    )
}

/// `POST /auth/token`: registered name, matching PEM, verifying signature,
/// single-use challenge — or 401.
fn mint_token(state: &Mutex<State>, request: &Request) -> Reply {
    let parsed = match serde_json::from_slice::<Value>(&request.body) {
        Ok(parsed) => parsed,
        Err(_) => return error_reply(400, "invalid_request", "body must be a JSON object"),
    };
    let Some(name) = parsed.get("name").and_then(Value::as_str) else {
        return error_reply(400, "invalid_request", "name is required");
    };
    let Some(challenge_id) = parsed.get("challenge_id").and_then(Value::as_str) else {
        return error_reply(400, "invalid_request", "challenge_id is required");
    };
    let Some(signature) = parsed.get("signature_base64").and_then(Value::as_str) else {
        return error_reply(400, "invalid_request", "signature_base64 is required");
    };
    let mut state = state.lock().unwrap();
    let Some(index) = state.challenges.iter().position(|c| c.id == challenge_id) else {
        return error_reply(401, "unauthorized", "unknown challenge");
    };
    let Some(bytes) = verifiable_bytes(&state, index, name) else {
        return error_reply(401, "unauthorized", "the name does not match the challenge");
    };
    let pem = state.challenges[index].pem.clone();
    if !signature_matches(&pem, &bytes, signature) {
        return error_reply(401, "unauthorized", "the signature does not verify");
    }
    state.challenges[index].used = true;
    state.tokens_minted += 1;
    let hex = hex_chars(&mut state.rng, 12);
    let secret = format!("tok-{}-{}", state.tokens_minted, hex);
    let expires_at = rfc3339(now_secs() + state.token_lifetime_secs);
    state.tokens.push(Token {
        secret: secret.clone(),
        member: name.to_string(),
        revoked: false,
    });
    json_reply(
        200,
        json!({ "token": secret, "token_expires_at": expires_at }),
    )
}

/// `POST /mcp`: record first, then refusal, then bearer auth, then dispatch.
/// A body that is not valid JSON is a `-32700` parse error, answered before
/// any bearer judgment. A JSON array is a batch, answered by the array of
/// the individual replies.
fn handle_mcp(state: &Mutex<State>, request: &Request) -> Reply {
    let bearer = request.bearer().unwrap_or_default().to_string();
    let accept = request.header("accept").map(str::to_string);
    let parsed = match serde_json::from_slice::<Value>(&request.body) {
        Ok(parsed) => parsed,
        Err(_) => {
            record_mcp(state, &bearer, Value::Null, &accept);
            return parse_error_reply();
        }
    };
    record_mcp(state, &bearer, parsed.clone(), &accept);
    {
        let mut state = state.lock().unwrap();
        if state.refuse_always || state.refuse_next > 0 {
            state.refuse_next = state.refuse_next.saturating_sub(1);
            return error_reply(401, "unauthorized", "the fake is refusing /mcp");
        }
    }
    let Some(caller) = authenticate(state, &bearer) else {
        return error_reply(
            401,
            "unauthorized",
            "the bearer is not a live token or API key",
        );
    };
    sse_wrapped(accept.as_deref(), dispatch_mcp(state, &caller, &parsed))
}

/// Record one `/mcp` request as it arrived, before any judgment.
fn record_mcp(state: &Mutex<State>, bearer: &str, body: Value, accept: &Option<String>) {
    state.lock().unwrap().mcp_seen.push(McpSeen {
        bearer: bearer.to_string(),
        body,
        accept: accept.clone(),
    });
}

/// The one parse-error reply for a body that is not valid JSON.
fn parse_error_reply() -> Reply {
    json_reply(
        400,
        json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32700, "message": "parse error" },
        }),
    )
}

/// Wrap a non-empty body in one SSE frame when the client asked for it.
fn sse_wrapped(accept: Option<&str>, reply: Reply) -> Reply {
    if accept == Some("text/event-stream") && !reply.body.is_empty() {
        Reply {
            status: reply.status,
            content_type: "text/event-stream",
            body: format!("event: message\ndata: {}\n\n", reply.body),
        }
    } else {
        reply
    }
}

/// JSON-RPC dispatch: a batch (array) is answered by the array of the
/// individual replies, notifications omitted; a lone notification is 202
/// with an empty body; everything else HTTP 200.
fn dispatch_mcp(state: &Mutex<State>, caller: &str, parsed: &Value) -> Reply {
    if let Value::Array(items) = parsed {
        let replies: Vec<Value> = items
            .iter()
            .filter_map(|item| dispatch_one(state, caller, item))
            .collect();
        if replies.is_empty() {
            return empty_reply(202);
        }
        return json_reply(200, Value::Array(replies));
    }
    dispatch_one(state, caller, parsed)
        .map_or_else(|| empty_reply(202), |reply| json_reply(200, reply))
}

/// One JSON-RPC message's reply value; `None` for a notification.
fn dispatch_one(state: &Mutex<State>, caller: &str, parsed: &Value) -> Option<Value> {
    let method = parsed
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method.starts_with("notifications/") {
        return None;
    }
    let id = parsed.get("id").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => initialize_result(parsed),
        "ping" => json!({}),
        "tools/list" => tools_list(),
        "tools/call" => call_tool(state, caller, parsed),
        other => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {other}") },
            }));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// `initialize`: echo a supported protocol version, else the fallback.
fn initialize_result(parsed: &Value) -> Value {
    let requested = parsed
        .pointer("/params/protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let version = if SUPPORTED_PROTOCOLS.contains(&requested) {
        requested
    } else {
        FALLBACK_PROTOCOL
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "fake-vivi-server", "version": "0" },
    })
}

fn tools_list() -> Value {
    let tool = |name: &str, description: &str, properties: Value| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": { "type": "object", "properties": properties },
        })
    };
    json!({
        "tools": [
            tool("echo", "echo", json!({ "text": { "type": "string" } })),
            tool("show", "show one item", json!({ "handle": { "type": "string" } })),
            tool(
                "thread",
                "show one item and its replies",
                json!({ "handle": { "type": "string" } }),
            ),
            tool(
                "reply",
                "append a reply to one item",
                json!({ "handle": { "type": "string" }, "body": { "type": "string" } }),
            ),
            tool("done", "mark one item done", json!({ "handle": { "type": "string" } })),
        ],
    })
}

/// A successful tool result carrying one text block.
fn tool_text(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": false })
}

/// A failed tool result: still HTTP 200, `isError` true.
fn tool_error(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

/// `tools/call`: `echo` reflects `<caller>: <text>`; the item tools read
/// and record against the seeded items, with the caller; unknown tools are
/// a tool-level error, still HTTP 200.
fn call_tool(state: &Mutex<State>, caller: &str, parsed: &Value) -> Value {
    let name = parsed
        .pointer("/params/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut state = state.lock().unwrap();
    match name {
        "echo" => {
            let text = parsed
                .pointer("/params/arguments/text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            tool_text(format!("{caller}: {text}"))
        }
        "show" | "thread" | "reply" | "done" => item_tool(&mut state, caller, name, parsed),
        other => tool_error(format!("unknown tool: {other}")),
    }
}

/// The item tools. `show` renders the item, `thread` the item and its
/// replies, `reply` appends a reply, `done` marks the item done; a reply
/// and a done are recorded as reports with the caller. A missing handle or
/// an unknown one is a tool-level error.
fn item_tool(state: &mut State, caller: &str, name: &str, parsed: &Value) -> Value {
    let Some(handle) = parsed
        .pointer("/params/arguments/handle")
        .and_then(Value::as_str)
    else {
        return tool_error(format!("{name} needs a handle"));
    };
    let Some(index) = state.items.iter().position(|item| item.handle == handle) else {
        return tool_error(format!("unknown item: {handle}"));
    };
    match name {
        "show" => tool_text(item_text(&state.items[index], false)),
        "thread" => tool_text(item_text(&state.items[index], true)),
        "reply" => {
            let body = parsed
                .pointer("/params/arguments/body")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            state.items[index].replies.push(ReplyRecord {
                by: caller.to_string(),
                body: body.clone(),
            });
            state.reports.push(Report {
                kind: "reply",
                item: handle.to_string(),
                by: caller.to_string(),
                body,
            });
            tool_text(format!("reply recorded on {handle} by {caller}"))
        }
        "done" => {
            state.items[index].state = "done".to_string();
            state.reports.push(Report {
                kind: "done",
                item: handle.to_string(),
                by: caller.to_string(),
                body: String::new(),
            });
            tool_text(format!("done {handle} by {caller}"))
        }
        _ => tool_error(format!("unknown item tool: {name}")),
    }
}

/// One item as the `show`/`thread` text: handle, subject, body and state,
/// and, for `thread`, the replies in order.
fn item_text(item: &Item, with_replies: bool) -> String {
    let mut text = format!(
        "handle: {}\nsubject: {}\nbody: {}\nstate: {}",
        item.handle, item.subject, item.body, item.state
    );
    if with_replies {
        text.push_str("\nreplies:");
        if item.replies.is_empty() {
            text.push_str(" none");
        }
        for reply in &item.replies {
            text.push_str(&format!("\n- {}: {}", reply.by, reply.body));
        }
    }
    text
}

/// The member name behind a live bearer: an unrevoked token, or the creator
/// of an unrevoked API key.
fn authenticate(state: &Mutex<State>, bearer: &str) -> Option<String> {
    let state = state.lock().unwrap();
    if let Some(token) = state
        .tokens
        .iter()
        .find(|token| token.secret == bearer && !token.revoked)
    {
        return Some(token.member.clone());
    }
    state
        .keys
        .iter()
        .find(|key| key.secret == bearer && !key.revoked)
        .map(|key| key.created_by.clone())
}

/// The member behind a live bearer, when the bearer must be a token.
fn token_member(state: &Mutex<State>, bearer: &str) -> Option<String> {
    let state = state.lock().unwrap();
    let token = state.tokens.iter().find(|token| token.secret == bearer)?;
    (!token.revoked).then(|| token.member.clone())
}

/// `POST /keys`: identity token only, 1..=64 char label, at most 32 live keys.
fn create_key(state: &Mutex<State>, request: &Request) -> Reply {
    let bearer = request.bearer().unwrap_or_default();
    let Some(member) = token_member(state, bearer) else {
        return error_reply(401, "unauthorized", "only an identity token can mint keys");
    };
    let parsed = match serde_json::from_slice::<Value>(&request.body) {
        Ok(parsed) => parsed,
        Err(_) => return error_reply(400, "invalid_request", "body must be a JSON object"),
    };
    let Some(label) = parsed.get("label").and_then(Value::as_str) else {
        return error_reply(400, "invalid_request", "label is required");
    };
    if label.is_empty() || label.chars().count() > 64 {
        return error_reply(400, "invalid_request", "label must be 1 to 64 characters");
    }
    let mut state = state.lock().unwrap();
    if state.keys.iter().filter(|key| !key.revoked).count() >= MAX_LIVE_KEYS {
        return error_reply(409, "key_limit", "at most 32 live keys are allowed");
    }
    state.seq += 1;
    let key_id = format!("key_{}", state.seq);
    let secret = format!("vivi_{}", hex_chars(&mut state.rng, 32));
    let created_at = rfc3339(now_secs());
    state.keys.push(StoredKey {
        key_id: key_id.clone(),
        label: label.to_string(),
        created_by: member.clone(),
        secret: secret.clone(),
        revoked: false,
        revoked_at: None,
        created_at: created_at.clone(),
    });
    json_reply(
        201,
        json!({
            "key_id": key_id,
            "key": secret,
            "label": label,
            "created_by": member,
            "created_at": created_at,
        }),
    )
}

/// `GET /keys`: identity token required; the list keeps revoked keys (each
/// with a `revoked_at`) so an audit can see them, live keys carry none.
/// Never a secret.
fn list_keys(state: &Mutex<State>, request: &Request) -> Reply {
    let bearer = request.bearer().unwrap_or_default();
    if token_member(state, bearer).is_none() {
        return error_reply(401, "unauthorized", "only an identity token can list keys");
    }
    let state = state.lock().unwrap();
    json_reply(200, keys_list_body(&state.keys))
}

/// The `/keys` list body, built here alone so a later unit can reshape it in
/// one place.
fn keys_list_body(keys: &[StoredKey]) -> Value {
    let listed: Vec<Value> = keys
        .iter()
        .map(|key| {
            let mut item = json!({
                "key_id": key.key_id,
                "label": key.label,
                "created_by": key.created_by,
                "created_at": key.created_at,
            });
            if let Some(at) = &key.revoked_at {
                item["revoked_at"] = json!(at);
            }
            item
        })
        .collect();
    json!({ "keys": listed })
}

/// `DELETE /keys/{key_id}`: the minting member revokes their key.
/// Idempotent on staging: revoking an already-revoked own key is 204 again.
/// Only an id that never existed, or another member's, is 404.
fn revoke_key(state: &Mutex<State>, request: &Request, key_id: &str) -> Reply {
    let bearer = request.bearer().unwrap_or_default();
    let Some(member) = token_member(state, bearer) else {
        return error_reply(
            401,
            "unauthorized",
            "only an identity token can revoke keys",
        );
    };
    let mut state = state.lock().unwrap();
    match state.keys.iter_mut().find(|key| key.key_id == key_id) {
        Some(key) if key.created_by == member => {
            key.revoked = true;
            if key.revoked_at.is_none() {
                key.revoked_at = Some(rfc3339(now_secs()));
            }
            empty_reply(204)
        }
        _ => error_reply(404, "key_not_found", "no such key minted by this member"),
    }
}

/// The challenge bytes when `name` is registered under the challenge's PEM
/// and the challenge is still single-use. The caller holds the state lock.
fn verifiable_bytes(state: &State, index: usize, name: &str) -> Option<Vec<u8>> {
    let challenge = &state.challenges[index];
    let member = state.members.iter().find(|member| member.name == name)?;
    (member.pem == challenge.pem && !challenge.used).then(|| challenge.bytes.clone())
}

/// Verify an ed25519 signature over `message`, keyed by the PEM's SPKI DER.
fn signature_matches(pem: &str, message: &[u8], signature_base64: &str) -> bool {
    let Some(key) = verifying_key_from_pem(pem) else {
        return false;
    };
    let Some(bytes) = b64::decode(signature_base64) else {
        return false;
    };
    let Ok(array) = <[u8; 64]>::try_from(bytes.as_slice()) else {
        return false;
    };
    key.verify(message, &Signature::from_bytes(&array)).is_ok()
}

/// The ed25519 public key: the last 32 bytes of the SPKI DER inside the PEM.
fn verifying_key_from_pem(pem: &str) -> Option<VerifyingKey> {
    let body: String = pem
        .lines()
        .filter(|line| !line.contains("-----"))
        .map(str::trim)
        .collect();
    let der = b64::decode(&body)?;
    if der.len() < 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&der[der.len() - 32..]);
    VerifyingKey::from_bytes(&key).ok()
}

/// 32 pairwise-distinct bytes, Fisher-Yates shuffled by an xorshift shift.
fn distinct_bytes(rng: &mut u64) -> [u8; 32] {
    let mut values = [0u8; 32];
    for (index, value) in values.iter_mut().enumerate() {
        *value = index as u8;
    }
    for index in (1..values.len()).rev() {
        mix(rng);
        let swap = (*rng % (index as u64 + 1)) as usize;
        values.swap(index, swap);
    }
    values
}

/// `count` lowercase hex chars drawn from the xorshift state.
fn hex_chars(rng: &mut u64, count: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    (0..count)
        .map(|_| {
            mix(rng);
            HEX[(*rng % 16) as usize] as char
        })
        .collect()
}

/// One xorshift64 step.
fn mix(rng: &mut u64) {
    *rng ^= *rng << 13;
    *rng ^= *rng >> 7;
    *rng ^= *rng << 17;
}

/// Unix seconds now.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

/// RFC 3339 UTC at second precision, from Unix seconds, std only.
fn rfc3339(unix_secs: i64) -> String {
    let (year, month, day) = civil_from_days(unix_secs.div_euclid(86_400));
    let second_of_day = unix_secs.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z",
        hour = second_of_day / 3_600,
        minute = (second_of_day % 3_600) / 60,
        second = second_of_day % 60,
    )
}

/// Days since the Unix epoch to (year, month, day); valid for negative days.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_pointer = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_pointer + 2) / 5 + 1) as u32;
    let month = if month_pointer < 10 {
        month_pointer + 3
    } else {
        month_pointer - 9
    } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

/// A tiny std TcpStream client so tests drive the fake without the binary.
pub fn http(
    url: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> HttpReply {
    let (host, port) = host_port(url);
    let mut stream = TcpStream::connect((host, port)).expect("connect to the fake server");
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .expect("set read timeout");
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("Connection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .expect("write request head");
    if let Some(body) = body {
        stream
            .write_all(body.as_bytes())
            .expect("write request body");
    }
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read reply");
    parse_reply(&raw)
}

/// Split `http://host:port/...` into host and port.
fn host_port(url: &str) -> (&str, u16) {
    let rest = url.strip_prefix("http://").unwrap_or(url);
    let authority = rest.split('/').next().unwrap_or(rest);
    match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().unwrap_or(80)),
        None => (authority, 80),
    }
}

/// Parse one HTTP reply read to EOF: status line, headers, body.
fn parse_reply(raw: &[u8]) -> HttpReply {
    let Some(head_end) = raw.windows(4).position(|window| window == b"\r\n\r\n") else {
        return HttpReply {
            status: 0,
            headers: Vec::new(),
            body: String::from_utf8_lossy(raw).into_owned(),
        };
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    HttpReply {
        status,
        headers,
        body: String::from_utf8_lossy(&raw[head_end + 4..]).into_owned(),
    }
}
