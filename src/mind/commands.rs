//! `vivi mind ...` dispatch: the thin attach client and the service verbs.
//!
//! Every client verb that needs the running mind goes through the socket.
//! Pause and resume also work with the mind down, by writing the mind's own
//! state file, which the service re-reads every tick.

use std::io::Write as _;
use std::path::Path;

use tokio::io::{AsyncBufReadExt, BufReader};

use crate::VivariumError;
use crate::cli::{MindCommand, MindServiceCommand, MindSubcommand};

use super::config::{self, MindConfig};
use super::projects;
use super::socket::{Client, Reply, Request};
use super::store;

/// The example systemd unit `vivi mind service unit` prints.
pub const UNIT_EXAMPLE: &str = include_str!("vivi-mind.service.example");

/// Dispatch one `vivi mind` invocation.
///
/// # Errors
/// As the subcommand.
pub async fn dispatch(command: &MindCommand) -> Result<(), VivariumError> {
    let sub = command.command.as_ref().unwrap_or(&MindSubcommand::Attach);
    if let MindSubcommand::Service { command } = sub {
        return service(command).await;
    }
    let config = config::load()?;
    match sub {
        MindSubcommand::Attach => attach(&config.paths.socket).await,
        MindSubcommand::Status { json } => status(&config, *json).await,
        MindSubcommand::Log { lines } => log(&config.paths.log, *lines),
        MindSubcommand::Pause => set_paused(&config, true).await,
        MindSubcommand::Resume => set_paused(&config, false).await,
        MindSubcommand::Stop => one_shot(&config.paths.socket, &Request::Stop).await,
        MindSubcommand::Start => Err(VivariumError::Other(
            "mind start is the service manager's job: it runs `vivi mind service run` (see `vivi mind service unit`)".into(),
        )),
        MindSubcommand::Compact => one_shot(&config.paths.socket, &Request::Compact).await,
        MindSubcommand::History { limit } => {
            one_shot(&config.paths.socket, &Request::History { limit: *limit }).await
        }
        MindSubcommand::Service { .. } => Ok(()),
    }
}

async fn service(command: &MindServiceCommand) -> Result<(), VivariumError> {
    match command {
        MindServiceCommand::Run => super::service::serve_forever().await,
        MindServiceCommand::Unit => {
            print!("{UNIT_EXAMPLE}");
            Ok(())
        }
        MindServiceCommand::Install | MindServiceCommand::Uninstall => Err(VivariumError::Other(
            "mind service install/uninstall is not implemented in this spike; `vivi mind service unit` prints an example systemd unit to review and install by hand".into(),
        )),
        MindServiceCommand::List => list(&config::load()?),
        MindServiceCommand::Add(cmd) => set_ignored(&config::load()?, &cmd.project, false),
        MindServiceCommand::Remove(cmd) => set_ignored(&config::load()?, &cmd.project, true),
        MindServiceCommand::Status => service_status(&config::load()?).await,
    }
}

fn list(config: &MindConfig) -> Result<(), VivariumError> {
    let state = store::load_at(&config.paths.state)?;
    let entries = projects::list_at(&crate::registry::path()?, &state.ignored)?;
    if entries.is_empty() {
        println!("no projects registered (see `vivi pump service add`)");
    }
    for entry in entries {
        let watched = entry.pumped && !entry.ignored;
        println!(
            "{}  watched={watched} pump={} ignored={}",
            entry.root.display(),
            entry.pumped,
            entry.ignored
        );
    }
    Ok(())
}

fn set_ignored(config: &MindConfig, project: &Path, ignored: bool) -> Result<(), VivariumError> {
    let root = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());
    let registered = crate::registry::load()?.get(&root).is_some();
    if !registered {
        return Err(VivariumError::Config(format!(
            "{} is not in the host registry; register it with `vivi pump service add`",
            root.display()
        )));
    }
    store::update_at(&config.paths.state, |state| {
        if ignored {
            state.ignored.insert(root.clone());
        } else {
            state.ignored.remove(&root);
        }
        Ok(())
    })?;
    let verb = if ignored { "ignoring" } else { "watching" };
    println!("{verb} {}", root.display());
    Ok(())
}

async fn service_status(config: &MindConfig) -> Result<(), VivariumError> {
    let listening = Client::connect(&config.paths.socket).await.is_ok();
    println!("listening {listening}");
    println!("authority {}", config.authority.as_str());
    println!("config {}", config.paths.config.display());
    println!("socket {}", config.paths.socket.display());
    println!("sessions {}", config.paths.session_dir.display());
    println!("state {}", config.paths.state.display());
    println!("log {}", config.paths.log.display());
    Ok(())
}

async fn status(config: &MindConfig, json: bool) -> Result<(), VivariumError> {
    let view = match Client::connect(&config.paths.socket).await {
        Ok(mut client) => client
            .collect(&Request::Status)
            .await?
            .into_iter()
            .find_map(|reply| match reply {
                Reply::Status { status } => Some(status),
                _ => None,
            }),
        Err(_) => None,
    };
    let running = view.is_some();
    let view = match view {
        Some(view) => view,
        None => offline_view(config)?,
    };
    if json {
        let mut value =
            serde_json::to_value(&view).map_err(|error| VivariumError::Parse(error.to_string()))?;
        value["running"] = serde_json::Value::Bool(running);
        println!("{value}");
        return Ok(());
    }
    print_status(&view, running);
    Ok(())
}

fn offline_view(config: &MindConfig) -> Result<super::socket::StatusView, VivariumError> {
    let state = store::load_at(&config.paths.state)?;
    Ok(super::socket::StatusView {
        authority: config.authority.as_str().to_string(),
        paused: state.paused,
        busy: false,
        session_id: state.session_id,
        last_wake: state.last_wake,
        ignored: state.ignored.len(),
    })
}

fn print_status(view: &super::socket::StatusView, running: bool) {
    println!("running {running}");
    println!("authority {}", view.authority);
    println!("paused {}", view.paused);
    println!("busy {}", view.busy);
    println!("session {}", view.session_id.as_deref().unwrap_or("none"));
    println!("ignored {}", view.ignored);
    match &view.last_wake {
        Some(wake) => println!(
            "last wake started {} ended {} outcome {}",
            wake.started_at,
            wake.ended_at
                .map_or_else(|| String::from("in-flight"), |at| at.to_string()),
            wake.outcome.as_deref().unwrap_or("none")
        ),
        None => println!("last wake none"),
    }
}

fn log(path: &Path, lines: usize) -> Result<(), VivariumError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("no mind log yet at {}", path.display());
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let all: Vec<&str> = text.lines().collect();
    for line in &all[all.len().saturating_sub(lines)..] {
        println!("{line}");
    }
    Ok(())
}

async fn set_paused(config: &MindConfig, paused: bool) -> Result<(), VivariumError> {
    let request = if paused {
        Request::Pause
    } else {
        Request::Resume
    };
    if let Ok(mut client) = Client::connect(&config.paths.socket).await {
        return client.request(&request, print_reply).await;
    }
    store::update_at(&config.paths.state, |state| {
        state.paused = paused;
        Ok(())
    })?;
    println!("mind not running; recorded paused={paused} in its state file");
    Ok(())
}

async fn one_shot(socket: &Path, request: &Request) -> Result<(), VivariumError> {
    Client::connect(socket)
        .await?
        .request(request, print_reply)
        .await
}

fn print_reply(reply: &Reply) {
    match reply {
        Reply::Text { text } => {
            print!("{text}");
            flush();
        }
        Reply::Ok { message } => println!("{message}"),
        Reply::Error { message } => eprintln!("error: {message}"),
        Reply::Status { status } => println!("{status:?}"),
        Reply::Done => {}
    }
}

fn flush() {
    if let Err(error) = std::io::stdout().flush() {
        tracing::debug!(%error, "stdout flush failed");
    }
}

/// The attach REPL: each line is a prompt to the one session; `/status`,
/// `/pause`, `/resume`, `/compact`, `/history` and `/quit` are verbs.
async fn attach(socket: &Path) -> Result<(), VivariumError> {
    let mut client = Client::connect(socket).await?;
    println!(
        "attached to vivi mind at {} (/quit to detach)",
        socket.display()
    );
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    loop {
        print!("mind> ");
        flush();
        let Some(line) = input.next_line().await? else {
            return Ok(());
        };
        let request = match line.trim() {
            "" => continue,
            "/quit" | "/exit" => return Ok(()),
            "/status" => Request::Status,
            "/pause" => Request::Pause,
            "/resume" => Request::Resume,
            "/compact" => Request::Compact,
            "/history" => Request::History { limit: 20 },
            text => Request::Prompt {
                text: text.to_string(),
            },
        };
        client.request(&request, print_reply).await?;
        println!();
    }
}
