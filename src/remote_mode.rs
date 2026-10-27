//! Transparent remote mode: when the nearest `.vivi/mailspace.toml` carries a
//! `[remote]` table, the ordinary CLI commands are translated into the
//! matching vivi-server MCP tool call instead of touching a local store.
//!
//! ```toml
//! [remote]
//! server = "http://hub.tailc3379f.ts.net:18190"
//! project = "faberlang"
//! key_file = "~/.vivi/faberlang-mcp.key"   # optional; else machine-key sign-in
//! ```
//!
//! A command with no mapping is refused by name; nothing falls back to local.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value, json};
use vivi::VivariumError;
use vivi::cli::{Command, MailCommand, NeedCommand, TaskStatus};
use vivi::mcp::HttpUpstream;
use vivi::mcp::client::{self, Client};

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RemoteConfig {
    pub server: String,
    pub project: String,
    #[serde(default)]
    pub key_file: Option<String>,
}

#[derive(Deserialize)]
struct ConfigFile {
    remote: Option<RemoteConfig>,
}

/// The `[remote]` table of the mailspace this command would open, if any.
pub(crate) fn remote_config(command: &Command) -> Result<Option<RemoteConfig>, VivariumError> {
    if matches!(command, Command::Mcp(_) | Command::Key { .. }) {
        return Ok(None);
    }
    let start = match command_project(command) {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir()?,
    };
    let Some(file) = start
        .ancestors()
        .map(|dir| dir.join(".vivi").join("mailspace.toml"))
        .find(|path| path.is_file())
    else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&file)?;
    let parsed: ConfigFile = toml::from_str(&text)
        .map_err(|e| VivariumError::Other(format!("{}: {e}", file.display())))?;
    Ok(parsed.remote)
}

/// Translate, call, print. A tool error exits 1.
pub(crate) fn run(command: &Command, remote: &RemoteConfig) -> Result<(), VivariumError> {
    let (tool, mut args) = translate(command)?;
    args.insert("project".into(), json!(remote.project));
    let server = remote.server.trim_end_matches('/').to_string();
    let attempt = |api_key: &str| -> Result<Value, String> {
        let upstream = HttpUpstream::new(&server);
        let mut mcp = Client::connect(&upstream, api_key).map_err(|e| e.to_string())?;
        mcp.call_tool(tool, &Value::Object(args.clone()))
            .map_err(|e| e.to_string())
    };
    let first = attempt(&api_key(remote)?);
    let result = match first {
        // A signed-in session the server no longer knows (it restarted):
        // sign in again, once.
        Err(e) if remote.key_file.is_none() && vivi::machine_key::is_refused_bearer(&e) => {
            vivi::machine_key::forget(&server, false)?;
            attempt(&api_key(remote)?)
        }
        other => other,
    }
    .map_err(|e| VivariumError::Other(format!("remote {server} {tool}: {e}")))?;
    let text = client::result_text(&result);
    let embedded_error =
        serde_json::from_str::<Value>(&text).is_ok_and(|v| v.get("error").is_some());
    if client::result_is_error(&result) || embedded_error {
        eprintln!("{text}");
        std::process::exit(1);
    }
    println!("{text}");
    Ok(())
}

fn api_key(remote: &RemoteConfig) -> Result<String, VivariumError> {
    if let Some(path) = &remote.key_file {
        let path = expand_home(path);
        let key = std::fs::read_to_string(&path)
            .map_err(|e| VivariumError::Other(format!("key_file {}: {e}", path.display())))?;
        return Ok(key.trim().to_string());
    }
    vivi::machine_key::sign_in(&remote.server, false)
        .map_err(|e| VivariumError::Other(format!("remote mode: machine sign-in: {e}")))
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => PathBuf::from(path),
    }
}

fn command_project(command: &Command) -> Option<&Path> {
    match command {
        Command::Need { command } => match command {
            NeedCommand::Send(c) => c.send.project.as_deref(),
            NeedCommand::List { project, .. }
            | NeedCommand::Show { project, .. }
            | NeedCommand::Done { project, .. }
            | NeedCommand::Reopen { project, .. } => project.as_deref(),
            _ => None,
        },
        Command::Mail { command } => match command {
            MailCommand::Reply(c) => c.project.as_deref(),
            MailCommand::Thread(c) => c.project.as_deref(),
            _ => None,
        },
        Command::Boot { project, .. } => project.as_deref(),
        _ => None,
    }
}

type Args = Map<String, Value>;

/// The arguments of `need send`.
fn put_need_send(c: &vivi::cli::NeedSendCommand, a: &mut Args) -> Result<(), VivariumError> {
    a.insert("kind".into(), json!("need"));
    a.insert("to".into(), json!(c.send.to));
    a.insert("subject".into(), json!(c.send.subject));
    a.insert(
        "body".into(),
        json!(read_body(
            c.send.body.as_deref(),
            c.send.body_file.as_deref()
        )?),
    );
    put_list(a, "done_when", &c.done_when);
    put_list(a, "depends_on", &c.depends_on);
    put_list(a, "write_scope", &c.write_scope);
    put(a, "target", c.target.as_deref());
    put(a, "serves", c.serves.as_deref());
    put(a, "explores", c.explores.as_deref());
    if c.dispatch {
        a.insert("dispatch".into(), json!(true));
    }
    if let Some(p) = c.priority {
        a.insert("priority".into(), json!(p));
    }
    Ok(())
}

/// The `need` subcommands: the tool and its arguments.
fn translate_need(command: &NeedCommand, a: &mut Args) -> Result<&'static str, VivariumError> {
    let tool = match command {
        NeedCommand::Send(c) => {
            put_need_send(c, a)?;
            "send"
        }
        NeedCommand::List {
            for_identity,
            from,
            to,
            status,
            marks,
            origin,
            ..
        } => {
            a.insert("kind".into(), json!("need"));
            put(a, "origin", origin.as_deref());
            put(a, "for", for_identity.as_deref());
            put(a, "from", from.as_deref());
            put(a, "to", to.as_deref());
            a.insert("status".into(), json!(status_word(status)));
            put_list(a, "marks", marks);
            "list"
        }
        NeedCommand::Show { handle, .. } => {
            a.insert("handle".into(), json!(handle));
            "show"
        }
        NeedCommand::Done {
            handle,
            for_identity,
            note,
            branch,
            commit,
            ..
        } => {
            a.insert("handle".into(), json!(handle));
            a.insert("for".into(), json!(for_identity));
            put(a, "note", note.as_deref());
            put(a, "branch", branch.as_deref());
            put(a, "commit", commit.as_deref());
            "done"
        }
        NeedCommand::Reopen {
            handle,
            for_identity,
            note,
            ..
        } => {
            a.insert("handle".into(), json!(handle));
            a.insert("for".into(), json!(for_identity));
            put(a, "note", note.as_deref());
            "reopen"
        }
        _ => return Err(unmapped(command_name(command))),
    };
    Ok(tool)
}

fn translate(command: &Command) -> Result<(&'static str, Args), VivariumError> {
    let mut a = Args::new();
    let tool = match command {
        Command::Need { command } => translate_need(command, &mut a)?,
        Command::Mail { command } => match command {
            MailCommand::Reply(c) => {
                a.insert("handle".into(), json!(c.handle));
                a.insert(
                    "body".into(),
                    json!(read_body(c.body.as_deref(), c.body_file.as_deref())?),
                );
                "reply"
            }
            MailCommand::Thread(c) => {
                a.insert("handle".into(), json!(c.handle));
                a.insert("limit".into(), json!(c.limit));
                "thread"
            }
            _ => return Err(unmapped(command_name(command))),
        },
        Command::Board(c) => {
            put(&mut a, "for", c.for_identity.as_deref());
            "board"
        }
        Command::Boot { .. } => "overview",
        other => return Err(unmapped(command_name(other))),
    };
    Ok((tool, a))
}

fn put(a: &mut Args, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        a.insert(key.into(), json!(value));
    }
}

fn put_list(a: &mut Args, key: &str, values: &[String]) {
    if !values.is_empty() {
        a.insert(key.into(), json!(values));
    }
}

fn status_word(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Open => "open",
        TaskStatus::Done => "done",
        TaskStatus::All => "all",
    }
}

fn read_body(body: Option<&str>, file: Option<&Path>) -> Result<String, VivariumError> {
    if let Some(path) = file {
        return Ok(std::fs::read_to_string(path)?);
    }
    match body {
        Some(text) if text.starts_with('@') => Ok(std::fs::read_to_string(&text[1..])?),
        Some(text) => Ok(text.to_string()),
        None => Ok(String::new()),
    }
}

fn command_name(command: &impl std::fmt::Debug) -> String {
    let text = format!("{command:?}");
    text.split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or("?")
        .to_string()
}

fn unmapped(name: String) -> VivariumError {
    VivariumError::Other(format!(
        "remote mode: `{name}` has no remote mapping yet ([remote] in mailspace.toml)"
    ))
}
