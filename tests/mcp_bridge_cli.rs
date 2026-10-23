//! The `vivi mcp` bridge as a child process against the HTTP fake of the
//! project server: identity credentials, API keys, token renewal, refusal
//! before any request, and stdout purity (every line is JSON-RPC 2.0).
//!
//! Stdout is read through a thread and a `recv_timeout`, so a hung bridge
//! fails the test instead of blocking CI.

#[path = "support/project_server.rs"]
mod project_server;

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use project_server::ProjectServer;

/// How long a reply or an exit may take before the test declares a hang.
const HANG: Duration = Duration::from_secs(20);

/// A running `vivi mcp` child: its stdin handle, its stdout lines arriving
/// over a channel, and its stderr draining into another channel.
struct Bridge {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    stderr: Receiver<String>,
}

impl Bridge {
    fn send_line(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").expect("write to the bridge stdin");
        self.stdin.flush().expect("flush the bridge stdin");
    }

    /// One stdout line, parsed. The line must be a JSON object speaking
    /// JSON-RPC 2.0, and it must arrive within [`HANG`].
    fn read_json(&mut self, context: &str) -> Value {
        let line = self
            .lines
            .recv_timeout(HANG)
            .unwrap_or_else(|_| panic!("no stdout line within 20s ({context}); the bridge hung"));
        let parsed: Value = serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("stdout is not JSON ({context}): {line}: {error}"));
        assert_eq!(
            parsed["jsonrpc"], "2.0",
            "stdout is not JSON-RPC 2.0 ({context}): {line}"
        );
        parsed
    }

    /// Send one request line and read its reply.
    fn exchange(&mut self, line: &str, context: &str) -> Value {
        self.send_line(line);
        self.read_json(context)
    }

    /// Close stdin, drain stdout to EOF through the same timeout reader,
    /// then wait for the child to exit on its own; every line still arriving
    /// must be a JSON object speaking JSON-RPC 2.0, and a child that will
    /// not die is killed and fails the test.
    fn finish(mut self) -> (ExitStatus, String) {
        drop(self.stdin);
        loop {
            match self.lines.recv_timeout(HANG) {
                Ok(line) => {
                    let parsed: Value = serde_json::from_str(&line).unwrap_or_else(|error| {
                        panic!("stdout after stdin closed is not JSON ({error}): {line}")
                    });
                    assert!(
                        parsed.is_object(),
                        "stdout after stdin closed is not a JSON object: {line}"
                    );
                    assert_eq!(
                        parsed["jsonrpc"], "2.0",
                        "stdout after stdin closed is not JSON-RPC 2.0: {line}"
                    );
                }
                Err(RecvTimeoutError::Timeout) => {
                    panic!(
                        "stdout did not reach EOF within 20s after stdin closed; the bridge hung"
                    );
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        let deadline = Instant::now() + HANG;
        loop {
            match self.child.try_wait().expect("poll the bridge") {
                Some(status) => {
                    let stderr = self
                        .stderr
                        .recv_timeout(HANG)
                        .unwrap_or_else(|_| panic!("no stderr within 20s after exit ({status})"));
                    return (status, stderr);
                }
                None if Instant::now() >= deadline => {
                    self.child.kill().expect("kill the hung bridge");
                    panic!("vivi mcp did not exit 20s after stdin closed");
                }
                None => thread::sleep(Duration::from_millis(20)),
            }
        }
    }
}

/// Spawn `vivi mcp` with piped stdio in `cwd`, its environment cleared of
/// every `VIVI_MCP_*` variable, then `args` and `envs` applied.
fn spawn(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Bridge {
    let mut child = mcp_command(cwd, args, envs)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn vivi mcp");
    let stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    let (lines_tx, lines_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });
    let stderr_pipe = child.stderr.take().expect("child stderr");
    let (stderr_tx, stderr_rx) = mpsc::channel();
    thread::spawn(move || {
        let text = std::io::read_to_string(stderr_pipe).unwrap_or_default();
        let _ = stderr_tx.send(text);
    });
    Bridge {
        child,
        stdin,
        lines: lines_rx,
        stderr: stderr_rx,
    }
}

/// A `vivi mcp` command for a run with no interactive stdio: stdin is null,
/// so the bridge sees an immediate EOF, and stdout/stderr are captured.
fn run_mcp(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    mcp_command(&cwd, args, envs)
        .output()
        .expect("run vivi mcp")
}

/// The `vivi mcp` command: cwd, cleared `VIVI_MCP_*` environment, args, envs.
fn mcp_command(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vivi"));
    command.args(["mcp"]).args(args).current_dir(cwd);
    for (name, _) in std::env::vars() {
        if name.starts_with("VIVI_MCP_") {
            command.env_remove(&name);
        }
    }
    for (name, value) in envs {
        command.env(name, value);
    }
    command
}

fn run_vivi(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .expect("run vivi");
    assert!(
        output.status.success(),
        "vivi {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A scratch mailspace whose own key is registered on the fake as `name`;
/// its seed file is `.vivi/chat/identity.ed25519`.
fn mailspace_with_registered_key(server: &ProjectServer, name: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("tempdir");
    run_vivi(&[
        "mailspace",
        "init",
        "--project",
        &root.path().to_string_lossy(),
    ]);
    let pem = run_vivi(&["sync", "key", "--project", &root.path().to_string_lossy()]);
    server.register(name, pem.trim());
    root
}

/// The scratch mailspace of [`mailspace_with_registered_key`] plus a
/// `[sync]` table naming the fake as the server.
fn mailspace_with_sync_table(server: &ProjectServer, name: &str) -> tempfile::TempDir {
    let root = mailspace_with_registered_key(server, name);
    let config = root.path().join(".vivi/mailspace.toml");
    let mut text = std::fs::read_to_string(&config).expect("read mailspace.toml");
    text.push_str(&format!(
        "\n[sync]\nserver = \"{}\"\nproject = \"{}\"\nname = \"{name}\"\n",
        server.url(),
        server.project()
    ));
    std::fs::write(&config, text).expect("write [sync] table");
    root
}

fn initialize_request(id: u64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"initialize","params":{{"protocolVersion":"2025-03-26","capabilities":{{}},"clientInfo":{{"name":"bridge-test","version":"0"}}}}}}"#
    )
}

fn assert_dir_is_empty(path: &Path, context: &str) {
    let entries: Vec<_> = std::fs::read_dir(path)
        .expect("read the directory")
        .collect();
    assert!(entries.is_empty(), "{context}: the directory is not empty");
}

fn assert_no_token(server: &ProjectServer, stderr: &str, context: &str) {
    for seen in server.mcp_requests() {
        assert!(
            !stderr.contains(&seen.bearer),
            "{context}: a bearer token leaked to stderr"
        );
    }
}

/// The identity flags every member-credential child run needs, pointing at
/// the scratch mailspace's seed file.
fn identity_flags(server: &ProjectServer, key_file: &Path) -> Vec<String> {
    vec![
        "--server".to_string(),
        server.url(),
        "--project-id".to_string(),
        server.project().to_string(),
        "--name".to_string(),
        "alice".to_string(),
        "--key-file".to_string(),
        key_file.to_string_lossy().into_owned(),
    ]
}

#[test]
fn identity_flags_only_in_an_empty_directory_serve_the_pass_through() {
    let server = ProjectServer::start();
    let scratch = mailspace_with_registered_key(&server, "alice");
    let key_file = scratch.path().join(".vivi/chat/identity.ed25519");
    let empty = tempfile::tempdir().expect("empty tempdir");

    let identity = identity_flags(&server, &key_file);
    let identity: Vec<&str> = identity.iter().map(String::as_str).collect();
    let mut bridge = spawn(empty.path(), &identity, &[]);

    let init = bridge.exchange(&initialize_request(1), "initialize");
    assert_eq!(init["id"], 1);
    assert!(init.get("error").is_none(), "{init}");
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");

    bridge.send_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    let ping = bridge.exchange(
        r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
        "ping after the notification",
    );
    assert_eq!(
        ping["id"], 2,
        "the line after the notification is the ping reply"
    );

    let tools = bridge.exchange(
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#,
        "tools/list",
    );
    assert_eq!(tools["result"]["tools"][0]["name"], "echo");

    let call = bridge.exchange(
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"echo","arguments":{"text":"hello"}}}"#,
        "tools/call",
    );
    assert_eq!(call["result"]["content"][0]["text"], "alice: hello");

    assert_eq!(server.tokens_minted(), 1, "one mint for the whole session");
    let (status, stderr) = bridge.finish();
    assert_eq!(
        status.code(),
        Some(0),
        "exit 0 on closed stdin; stderr:\n{stderr}"
    );
    assert_dir_is_empty(empty.path(), "a run with flags only must create nothing");
    assert_no_token(&server, &stderr, "identity run");
}

#[test]
fn a_401_renews_once_and_the_call_still_is_answered() {
    let server = ProjectServer::start();
    let scratch = mailspace_with_registered_key(&server, "alice");
    let key_file = scratch.path().join(".vivi/chat/identity.ed25519");
    let empty = tempfile::tempdir().expect("empty tempdir");

    let identity = identity_flags(&server, &key_file);
    let identity: Vec<&str> = identity.iter().map(String::as_str).collect();
    let mut bridge = spawn(empty.path(), &identity, &[]);
    bridge.exchange(&initialize_request(1), "initialize");
    server.refuse_mcp_next(1);

    let call = bridge.exchange(
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{"text":"again"}}}"#,
        "tools/call after one 401",
    );

    assert_eq!(call["id"], 2);
    assert!(call.get("error").is_none(), "{call}");
    assert_eq!(call["result"]["content"][0]["text"], "alice: again");
    assert_eq!(
        server.tokens_minted(),
        2,
        "the session's first mint plus the one renewal"
    );
    let (status, stderr) = bridge.finish();
    assert_eq!(status.code(), Some(0), "{stderr}");
}

#[test]
fn a_server_that_keeps_refusing_answers_errors_and_keeps_serving() {
    let server = ProjectServer::start();
    let scratch = mailspace_with_registered_key(&server, "alice");
    let key_file = scratch.path().join(".vivi/chat/identity.ed25519");
    let empty = tempfile::tempdir().expect("empty tempdir");

    let identity = identity_flags(&server, &key_file);
    let identity: Vec<&str> = identity.iter().map(String::as_str).collect();
    let mut bridge = spawn(empty.path(), &identity, &[]);
    server.refuse_mcp_always();

    let numeric = bridge.exchange(
        r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#,
        "numeric id",
    );
    assert_eq!(
        numeric["id"], 7,
        "the error carries the request's numeric id"
    );
    assert_eq!(numeric["error"]["code"], -32000);
    assert_eq!(numeric["error"]["data"]["http_status"], 401);
    let textual = bridge.exchange(
        r#"{"jsonrpc":"2.0","id":"seven","method":"tools/list"}"#,
        "string id",
    );
    assert_eq!(
        textual["id"], "seven",
        "the error carries the request's string id"
    );
    assert_eq!(textual["error"]["code"], -32000);
    let next = bridge.exchange(
        r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#,
        "next request",
    );
    assert_eq!(next["id"], 9, "the child keeps serving after refusals");

    let (status, stderr) = bridge.finish();
    assert_eq!(
        status.code(),
        Some(0),
        "exit 0 on closed stdin; stderr:\n{stderr}"
    );
    let requests = server.mcp_requests().len();
    assert!(
        server.tokens_minted() <= 2 * requests,
        "at most two mints per request: {} mints for {requests} requests",
        server.tokens_minted()
    );
}

#[test]
fn an_api_key_in_the_environment_serves_without_minting() {
    let server = ProjectServer::start();
    let key = server.issue_api_key("alice");
    let empty = tempfile::tempdir().expect("empty tempdir");

    let mut bridge = spawn(
        empty.path(),
        &[],
        &[
            ("VIVI_MCP_API_KEY", key.as_str()),
            ("VIVI_MCP_SERVER", &server.url()),
            ("VIVI_MCP_PROJECT_ID", server.project()),
        ],
    );

    let init = bridge.exchange(&initialize_request(1), "initialize with an api key");
    assert_eq!(init["id"], 1);
    assert!(init.get("error").is_none(), "{init}");
    assert_eq!(server.tokens_minted(), 0, "an API key never mints");

    server.refuse_mcp_always();
    let refused = bridge.exchange(
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        "refused api key",
    );
    assert_eq!(refused["error"]["code"], -32000);
    assert_eq!(refused["error"]["data"]["http_status"], 401);
    assert_eq!(server.tokens_minted(), 0, "still no mint after the refusal");
    let seen = server.mcp_requests();
    assert!(!seen.is_empty(), "the bridge reached the server");
    for request in &seen {
        assert_eq!(request.path, "/v1/mcp", "the server's MCP endpoint");
    }

    let (status, stderr) = bridge.finish();
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_dir_is_empty(empty.path(), "an environment-only run must create nothing");
}

#[test]
fn inside_a_mailspace_with_a_sync_table_no_flags_are_needed() {
    let server = ProjectServer::start();
    let root = mailspace_with_sync_table(&server, "alice");
    let empty = tempfile::tempdir().expect("empty tempdir");

    let mut bridge = spawn(
        empty.path(),
        &["--project", &root.path().to_string_lossy()],
        &[],
    );

    let init = bridge.exchange(&initialize_request(1), "initialize from [sync]");
    assert_eq!(init["id"], 1);
    assert!(init.get("error").is_none(), "{init}");
    let call = bridge.exchange(
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{"text":"hi"}}}"#,
        "tools/call from [sync]",
    );
    assert_eq!(call["result"]["content"][0]["text"], "alice: hi");

    let (status, stderr) = bridge.finish();
    assert_eq!(status.code(), Some(0), "{stderr}");
}

#[test]
fn no_server_anywhere_is_refused_and_names_every_source() {
    let server = ProjectServer::start();
    let scratch = mailspace_with_registered_key(&server, "alice");
    let key_file = scratch.path().join(".vivi/chat/identity.ed25519");
    let empty = tempfile::tempdir().expect("empty tempdir");

    let output = run_mcp(
        empty.path(),
        &[
            "--project-id",
            server.project(),
            "--name",
            "alice",
            "--key-file",
            &key_file.to_string_lossy(),
        ],
        &[],
    );

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout must stay empty: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--server"), "{stderr}");
    assert!(stderr.contains("VIVI_MCP_SERVER"), "{stderr}");
    assert!(stderr.contains("[sync] server"), "{stderr}");
    assert_dir_is_empty(empty.path(), "a refused run must create nothing");
}

#[test]
fn an_api_key_plus_a_member_name_is_refused() {
    let server = ProjectServer::start();
    let key = server.issue_api_key("alice");
    let empty = tempfile::tempdir().expect("empty tempdir");

    let output = run_mcp(
        empty.path(),
        &[
            "--server",
            &server.url(),
            "--project-id",
            server.project(),
            "--name",
            "alice",
        ],
        &[("VIVI_MCP_API_KEY", key.as_str())],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not both"), "{stderr}");
    assert!(output.stdout.is_empty(), "stdout must stay empty");
}

#[test]
fn a_missing_key_file_is_refused_before_any_request() {
    let server = ProjectServer::start();
    let empty = tempfile::tempdir().expect("empty tempdir");
    let missing = empty.path().join("no-such-key");

    let output = run_mcp(
        empty.path(),
        &[
            "--server",
            &server.url(),
            "--project-id",
            server.project(),
            "--name",
            "alice",
            "--key-file",
            &missing.to_string_lossy(),
        ],
        &[],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no-such-key"), "{stderr}");
    assert!(
        server.mcp_requests().is_empty(),
        "no request may reach the server"
    );
    assert_eq!(server.tokens_minted(), 0);
}
