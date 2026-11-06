//! The `vivi mcp` command: resolve flags, environment, and the mailspace
//! fallback, then serve the bridge on locked stdin and stdout, diagnostics
//! on stderr, until stdin closes. `tools` and `call` are one-shot clients
//! over the same credentials: they exit 1 when a tool reports an error and
//! 2 on a transport, auth, or JSON-RPC error.

use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;
use vivi_core::cli::{McpCommand, McpSubcommand};
use vivi_core::mcp::client::{self, Client};
use vivi_core::mcp::resolve::{self, MailspaceSource, McpFlags};
use vivi_core::mcp::{BridgeConfig, HttpUpstream, serve};
use vivi_error::VivariumError;

/// Exit status for a tool that reports `isError: true`.
const EXIT_TOOL_ERROR: i32 = 1;

/// Exit status for a transport, auth, JSON-RPC, or usage error.
const EXIT_FAILURE: i32 = 2;

pub(crate) fn handle_mcp_command(command: &McpCommand) -> Result<(), VivariumError> {
    let Some(one_shot) = &command.command else {
        return serve_quietly(&bridge_config(command)?);
    };
    let code = match run_one_shot(command, one_shot) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("vivi mcp: {message}");
            EXIT_FAILURE
        }
    };
    if code == 0 {
        Ok(())
    } else {
        std::process::exit(code);
    }
}

/// Run `tools` or `call` and return its exit status; an `Err` is a
/// failure to report with status 2. Arguments are checked before the
/// credentials are resolved, so a bad argument sends no request.
fn run_one_shot(command: &McpCommand, one_shot: &McpSubcommand) -> Result<i32, String> {
    let arguments = match one_shot {
        McpSubcommand::Tools { .. } => None,
        McpSubcommand::Call {
            args_json,
            args_file,
            ..
        } => Some(read_arguments(args_json.as_deref(), args_file.as_deref())?),
    };
    let config = bridge_config(command).map_err(|error| error.to_string())?;
    let signed_in = std::env::var(resolve::ENV_API_KEY).map_or(true, |key| key.is_empty());
    let upstream = HttpUpstream::new(&config.server);
    let retry_key: String;
    let mut mcp = match Client::connect(&upstream, &config.api_key) {
        Ok(mcp) => mcp,
        // A signed-in session the server no longer knows (it restarted):
        // sign in again, once. Any other failure is reported as it is.
        Err(error)
            if signed_in && vivi_core::machine_key::is_refused_bearer(&error.to_string()) =>
        {
            vivi_core::machine_key::forget(&config.server, command.host)
                .map_err(|e| e.to_string())?;
            retry_key = vivi_core::machine_key::sign_in(&config.server, command.host)
                .map_err(|e| e.to_string())?;
            Client::connect(&upstream, &retry_key).map_err(|e| e.to_string())?
        }
        Err(error) => return Err(error.to_string()),
    };
    match (one_shot, arguments) {
        (McpSubcommand::Tools { json }, _) => {
            let tools = mcp.tools().map_err(|error| error.to_string())?;
            let text = if *json {
                serde_json::to_string_pretty(&tools).map_err(|error| error.to_string())?
            } else {
                client::tool_lines(&tools).join("\n")
            };
            print_stdout(&text)?;
            Ok(0)
        }
        (McpSubcommand::Call { tool, json, .. }, Some(arguments)) => {
            let arguments =
                project_arguments(&mut mcp, tool, arguments, config.project.as_deref())?;
            let result = mcp
                .call_tool(tool, &arguments)
                .map_err(|error| error.to_string())?;
            print_call_result(&result, *json)
        }
        (McpSubcommand::Call { .. }, None) => Err("no arguments were read".into()),
    }
}

/// The arguments of a `call`, with the resolved project added when the
/// tool takes one and the arguments carry none. With no project, or an
/// explicit `project`, no `tools/list` is sent.
fn project_arguments(
    mcp: &mut Client<'_>,
    tool: &str,
    arguments: Value,
    project: Option<&str>,
) -> Result<Value, String> {
    let Some(project) = project else {
        return Ok(arguments);
    };
    if arguments.get("project").is_some() {
        return Ok(arguments);
    }
    let tools = mcp.tools().map_err(|error| error.to_string())?;
    if client::takes_project(&tools, tool) {
        Ok(client::with_project(&arguments, project))
    } else {
        Ok(arguments)
    }
}

/// Print a tool result and pick the exit status: the text (or the whole
/// result with `--json`) on stdout; a tool error also puts its text on
/// stderr and exits 1.
fn print_call_result(result: &Value, json: bool) -> Result<i32, String> {
    let text = client::result_text(result);
    if json {
        let full = serde_json::to_string_pretty(result).map_err(|error| error.to_string())?;
        print_stdout(&full)?;
    }
    if client::result_is_error(result) {
        eprintln!("{text}");
        return Ok(EXIT_TOOL_ERROR);
    }
    if !json {
        print_stdout(&text)?;
    }
    Ok(0)
}

fn print_stdout(text: &str) -> Result<(), String> {
    let mut out = io::stdout().lock();
    writeln!(out, "{text}")
        .and_then(|()| out.flush())
        .map_err(|error| format!("cannot write to stdout: {error}"))
}

/// The arguments object: `ARGS_JSON`, or the `--args-file` text (`-` is
/// stdin), or `{}`; it must parse to a JSON object.
fn read_arguments(inline: Option<&str>, file: Option<&str>) -> Result<Value, String> {
    let text = match (inline, file) {
        (Some(text), _) => text.to_string(),
        (None, Some("-")) => {
            let mut text = String::new();
            io::stdin()
                .read_to_string(&mut text)
                .map_err(|error| format!("cannot read arguments from stdin: {error}"))?;
            text
        }
        (None, Some(path)) => std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read --args-file {path}: {error}"))?,
        (None, None) => "{}".to_string(),
    };
    client::parse_arguments(&text)
}

/// Resolve flags, environment, and the mailspace fallback into the API key
/// credential, the server settings, and the project id if one resolved.
fn bridge_config(command: &McpCommand) -> Result<BridgeConfig, VivariumError> {
    let flags = McpFlags {
        server: command.server.clone(),
        project_id: command.project_id.clone(),
        project: command.project.clone(),
    };
    const SIGN_IN: &str = "\u{0}machine-sign-in";
    let env_lookup = |name: &str| match std::env::var(name) {
        Ok(value) if !value.is_empty() => Some(value),
        Ok(_) | Err(std::env::VarError::NotPresent | std::env::VarError::NotUnicode(_)) => {
            (name == resolve::ENV_API_KEY).then(|| SIGN_IN.to_string())
        }
    };
    let mailspace_lookup = || mailspace_source(command.project.as_deref());
    let resolved = resolve::resolve(&flags, env_lookup, mailspace_lookup)?;
    let machine_session = resolved.api_key == SIGN_IN;
    let api_key = if machine_session {
        vivi_core::machine_key::sign_in(&resolved.server, command.host)?
    } else {
        resolved.api_key
    };
    Ok(BridgeConfig {
        server: resolved.server,
        api_key,
        project: resolved.project,
        machine_host: machine_session.then_some(command.host),
    })
}

/// Serve one session. A write to a stdout whose reader is gone ends the
/// bridge as quietly as a closed stdin.
fn serve_quietly(config: &BridgeConfig) -> Result<(), VivariumError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let served = serve(
        config,
        BufReader::new(stdin.lock()),
        stdout.lock(),
        stderr.lock(),
    );
    match served {
        Err(VivariumError::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    }
}

/// What the mailspace named by `--project` (or the nearest ancestor of the
/// current directory holding `.vivi/mailspace.toml`) offers: `[remote]`
/// server and project, else `[sync]`, and for the server alone the local
/// server home. An absent mailspace is not an error: the bridge may run with
/// no mailspace at all, and nothing is created here.
fn mailspace_source(project: Option<&Path>) -> Result<Option<MailspaceSource>, VivariumError> {
    let root = match project {
        Some(root) => Some(root.to_path_buf()),
        None => mailspace_root(std::env::current_dir()?),
    };
    let (mut server, mut project) = (None, None);
    if let Some(root) = root {
        // Read the values straight from the file: opening the mailspace
        // would repair and rewrite it, and a read must not write.
        #[derive(serde::Deserialize, Default)]
        struct Table {
            server: Option<String>,
            project: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct File {
            remote: Option<Table>,
            sync: Option<Table>,
        }
        let path = root.join(".vivi").join("mailspace.toml");
        let text = std::fs::read_to_string(&path)?;
        let file: File = toml::from_str(&text)
            .map_err(|e| VivariumError::Other(format!("{}: {e}", path.display())))?;
        let remote = file.remote.unwrap_or_default();
        let sync = file.sync.unwrap_or_default();
        server = remote.server.or(sync.server);
        project = remote.project.or(sync.project);
    }
    let server = server.or_else(local_server_url);
    if server.is_none() && project.is_none() {
        return Ok(None);
    }
    Ok(Some(MailspaceSource { server, project }))
}

/// The URL of the server whose home is `$VIVI_HOME/server`, when its
/// `config.toml` exists: `http://<bind>:<port>`, a wildcard bind meaning
/// loopback. Only `bind` and `port` are read.
fn local_server_url() -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Config {
        bind: Option<String>,
        port: Option<u16>,
    }
    let Ok(home) = vivi_util::home::server_home() else {
        return None;
    };
    let Ok(text) = std::fs::read_to_string(home.join("config.toml")) else {
        return None;
    };
    let Ok(config) = toml::from_str::<Config>(&text) else {
        return None;
    };
    let bind = match config.bind.as_deref() {
        None | Some("" | "0.0.0.0" | "::" | "[::]") => "127.0.0.1".to_string(),
        Some(bind) if bind.contains(':') && !bind.starts_with('[') => format!("[{bind}]"),
        Some(bind) => bind.to_string(),
    };
    Some(format!("http://{bind}:{}", config.port.unwrap_or(8080)))
}

/// The nearest ancestor of `start` holding `.vivi/mailspace.toml`.
fn mailspace_root(start: PathBuf) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|path| path.join(".vivi").join("mailspace.toml").is_file())
        .map(Path::to_path_buf)
}
