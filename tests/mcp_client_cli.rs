//! `vivi mcp tools` and `vivi mcp call` as child processes against the HTTP
//! fake of the project server: listing, text and JSON results, the exit
//! statuses (0 ok, 1 tool error, 2 transport / JSON-RPC / usage), arguments
//! refused before any request, and that bare `vivi mcp` still parses as
//! the stdio bridge.

#[path = "support/project_server.rs"]
mod project_server;

use std::io::Write;
use std::process::{Command, Output, Stdio};

use clap::Parser;
use serde_json::Value;
use vivi::cli::{Cli, Command as CliCommand, McpSubcommand};

use project_server::ProjectServer;

/// Run `vivi mcp <args>` with an API key for `alice` in the environment,
/// every other `VIVI_MCP_*` variable cleared, and `stdin` piped in.
fn run(server: &ProjectServer, key: &str, args: &[&str], stdin: &str) -> Output {
    run_with_project(server, key, Some(server.project()), args, stdin)
}

/// `run` with the `VIVI_MCP_PROJECT_ID` environment variable given, or not.
fn run_with_project(
    server: &ProjectServer,
    key: &str,
    project: Option<&str>,
    args: &[&str],
    stdin: &str,
) -> Output {
    let empty = tempfile::tempdir().expect("empty tempdir");
    let mut command = Command::new(env!("CARGO_BIN_EXE_vivi"));
    command.arg("mcp").args(args).current_dir(empty.path());
    for (name, _) in std::env::vars() {
        if name.starts_with("VIVI_MCP_") {
            command.env_remove(&name);
        }
    }
    command
        .env("VIVI_MCP_API_KEY", key)
        .env("VIVI_MCP_SERVER", server.url())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(project) = project {
        command.env("VIVI_MCP_PROJECT_ID", project);
    }
    let mut child = command.spawn().expect("spawn vivi mcp");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait for vivi mcp")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn fixture() -> (ProjectServer, String) {
    let server = ProjectServer::start();
    let key = server.issue_api_key("alice");
    (server, key)
}

#[test]
fn tools_lists_each_name_with_its_description() {
    let (server, key) = fixture();
    let output = run(&server, &key, &["tools"], "");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.lines().any(|line| line == "echo\techo"), "{text}");
    assert!(
        text.lines().any(|line| line == "show\tshow one item"),
        "{text}"
    );
    assert!(!text.contains(&key), "the key must never be printed");
}

#[test]
fn tools_json_prints_the_raw_array() {
    let (server, key) = fixture();
    let output = run(&server, &key, &["tools", "--json"], "");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let tools: Value = serde_json::from_str(&stdout(&output)).expect("stdout is JSON");
    let tools = tools.as_array().expect("an array of tools");
    assert_eq!(tools.len(), 5);
    assert_eq!(tools[0]["name"], "echo");
    assert!(tools[0].get("inputSchema").is_some());
}

#[test]
fn call_prints_the_result_text() {
    let (server, key) = fixture();
    let output = run(&server, &key, &["call", "echo", r#"{"text":"hi"}"#], "");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "alice: hi\n");
    assert!(!stderr(&output).contains(&key));
    for seen in server.mcp_requests() {
        assert_eq!(seen.path, "/v1/mcp", "the server's MCP endpoint");
    }
    let methods: Vec<String> = server
        .mcp_requests()
        .iter()
        .map(|seen| seen.body["method"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/call"
        ]
    );
}

fn methods(server: &ProjectServer) -> Vec<String> {
    server
        .mcp_requests()
        .iter()
        .map(|seen| seen.body["method"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn last_call_arguments(server: &ProjectServer) -> Value {
    let seen = server.mcp_requests();
    let call = seen
        .iter()
        .rev()
        .find(|seen| seen.body["method"] == "tools/call")
        .expect("a tools/call request");
    call.body["params"]["arguments"].clone()
}

#[test]
fn call_injects_the_env_project_for_a_tool_that_takes_one() {
    let (server, key) = fixture();
    server.seed_item("t1", "first", "the body");
    let output = run(&server, &key, &["call", "show", r#"{"handle":"t1"}"#], "");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("subject: first"),
        "{}",
        stdout(&output)
    );
    assert_eq!(last_call_arguments(&server)["project"], server.project());
}

#[test]
fn call_sends_no_project_to_a_tool_that_does_not_take_one() {
    let (server, key) = fixture();
    let output = run(&server, &key, &["call", "echo", r#"{"text":"hi"}"#], "");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(last_call_arguments(&server).get("project").is_none());
}

#[test]
fn call_keeps_an_explicit_project() {
    let (server, key) = fixture();
    server.seed_item("t1", "first", "the body");
    let arguments = r#"{"project":"other","handle":"t1"}"#;
    let output = run(&server, &key, &["call", "show", arguments], "");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("this key has no grant on other"),
        "{}",
        stderr(&output)
    );
    assert_eq!(last_call_arguments(&server)["project"], "other");
    assert!(!methods(&server).contains(&"tools/list".to_string()));
}

#[test]
fn call_uses_the_project_id_flag_without_the_env() {
    let (server, key) = fixture();
    server.seed_item("t1", "first", "the body");
    let id = server.project();
    let output = run_with_project(
        &server,
        &key,
        None,
        &["call", "show", r#"{"handle":"t1"}"#, "--project-id", id],
        "",
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(last_call_arguments(&server)["project"], id);
}

#[test]
fn call_with_no_project_anywhere_sends_no_tools_list() {
    let (server, key) = fixture();
    server.seed_item("t1", "first", "the body");
    let output = run_with_project(
        &server,
        &key,
        None,
        &["call", "show", r#"{"handle":"t1"}"#],
        "",
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("project is required"),
        "{}",
        stderr(&output)
    );
    assert!(!methods(&server).contains(&"tools/list".to_string()));
    assert!(last_call_arguments(&server).get("project").is_none());
}

#[test]
fn tools_sends_no_project_and_no_call() {
    let (server, key) = fixture();
    let output = run(&server, &key, &["tools"], "");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(!methods(&server).contains(&"tools/call".to_string()));
    for seen in server.mcp_requests() {
        assert!(seen.body.pointer("/params/project").is_none());
    }
}

#[test]
fn call_json_prints_the_full_result_object() {
    let (server, key) = fixture();
    let output = run(
        &server,
        &key,
        &["call", "echo", r#"{"text":"hi"}"#, "--json"],
        "",
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let result: Value = serde_json::from_str(&stdout(&output)).expect("stdout is JSON");
    assert_eq!(result["isError"], false);
    assert_eq!(result["content"][0]["text"], "alice: hi");
}

#[test]
fn call_reads_arguments_from_stdin_and_from_a_file() {
    let (server, key) = fixture();
    let piped = run(
        &server,
        &key,
        &["call", "echo", "--args-file", "-"],
        r#"{"text":"piped"}"#,
    );
    assert_eq!(piped.status.code(), Some(0), "{}", stderr(&piped));
    assert_eq!(stdout(&piped), "alice: piped\n");

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("args.json");
    std::fs::write(&path, r#"{"text":"filed"}"#).expect("write args file");
    let filed = run(
        &server,
        &key,
        &["call", "echo", "--args-file", &path.to_string_lossy()],
        "",
    );
    assert_eq!(filed.status.code(), Some(0), "{}", stderr(&filed));
    assert_eq!(stdout(&filed), "alice: filed\n");
}

#[test]
fn call_without_arguments_sends_an_empty_object() {
    let (server, key) = fixture();
    let output = run(&server, &key, &["call", "echo"], "");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let seen = server.mcp_requests();
    let call = seen.last().expect("a tools/call request");
    assert_eq!(call.body["params"]["arguments"], serde_json::json!({}));
}

#[test]
fn a_tool_error_exits_1_with_its_text_on_stderr() {
    let (server, key) = fixture();
    let arguments = format!(r#"{{"handle":"nope","project":"{}"}}"#, server.project());
    let output = run(&server, &key, &["call", "show", &arguments], "");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("unknown item: nope"),
        "{}",
        stderr(&output)
    );
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
}

#[test]
fn a_json_rpc_error_exits_2_with_code_and_message() {
    let (server, key) = fixture();
    let output = run(&server, &key, &["call", "rpc_error"], "");
    assert_eq!(output.status.code(), Some(2), "{}", stdout(&output));
    let text = stderr(&output);
    assert!(text.contains("-32602"), "{text}");
    assert!(text.contains("invalid params from the fake"), "{text}");
}

#[test]
fn a_refused_credential_exits_2_naming_the_http_status() {
    let (server, key) = fixture();
    server.refuse_mcp_always();
    let output = run(&server, &key, &["tools"], "");
    assert_eq!(output.status.code(), Some(2), "{}", stdout(&output));
    let text = stderr(&output);
    assert!(text.contains("HTTP 401"), "{text}");
    assert!(!text.contains(&key), "the key must never be printed");
}

#[test]
fn arguments_that_are_not_an_object_are_refused_before_any_request() {
    let (server, key) = fixture();
    for bad in ["[1,2]", "\"text\"", "7", "{not json"] {
        let output = run(&server, &key, &["call", "echo", bad], "");
        assert_eq!(output.status.code(), Some(2), "{bad}: {}", stderr(&output));
        assert!(!stderr(&output).is_empty(), "{bad}");
    }
    let output = run(&server, &key, &["call", "echo", "--args-file", "-"], "[]");
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(server.mcp_requests().is_empty(), "no request may be sent");
}

#[test]
fn bare_mcp_still_parses_as_the_bridge() {
    let bare = Cli::try_parse_from(["vivi", "mcp"]).expect("bare mcp parses");
    let CliCommand::Mcp(command) = bare.command else {
        panic!("not the mcp command");
    };
    assert!(command.command.is_none());

    let flagged = Cli::try_parse_from(["vivi", "mcp", "--server", "http://s", "--project-id", "p"])
        .expect("bridge flags parse");
    let CliCommand::Mcp(command) = flagged.command else {
        panic!("not the mcp command");
    };
    assert!(command.command.is_none());
    assert_eq!(command.server.as_deref(), Some("http://s"));
    assert_eq!(command.project_id.as_deref(), Some("p"));
}

#[test]
fn credential_flags_work_after_the_subcommand_too() {
    let parsed = Cli::try_parse_from([
        "vivi",
        "mcp",
        "call",
        "echo",
        "{}",
        "--server",
        "http://s",
        "--project-id",
        "p",
    ])
    .expect("flags after the subcommand parse");
    let CliCommand::Mcp(command) = parsed.command else {
        panic!("not the mcp command");
    };
    assert_eq!(command.server.as_deref(), Some("http://s"));
    assert_eq!(command.project_id.as_deref(), Some("p"));
    assert!(matches!(command.command, Some(McpSubcommand::Call { .. })));

    let early = Cli::try_parse_from(["vivi", "mcp", "--server", "http://s", "tools"])
        .expect("flags before the subcommand parse");
    let CliCommand::Mcp(command) = early.command else {
        panic!("not the mcp command");
    };
    assert_eq!(command.server.as_deref(), Some("http://s"));
    assert!(matches!(command.command, Some(McpSubcommand::Tools { .. })));
}

#[test]
fn args_json_and_args_file_conflict() {
    let result = Cli::try_parse_from(["vivi", "mcp", "call", "echo", "{}", "--args-file", "-"]);
    assert!(result.is_err());
}
