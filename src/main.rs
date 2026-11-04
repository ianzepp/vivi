#![deny(clippy::pedantic)]

//! The `vivi` binary.

use std::process;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use vivi_core::cli::{
    Cli, Command, ProxySubcommand, PumpServiceCommand, PumpSubcommand, ServerSubcommand,
};
use vivi_error::VivariumError;
use vivi_update::update::CheckOutcome;

mod local_board_command;
mod local_boot_command;
mod local_config_command;
mod local_delivery_command;
mod local_doctor_command;
mod local_flow_command;
mod local_goal_command;
mod local_graph_command;
mod local_mail_list;
mod local_mailspace_command;
mod local_mailspace_dump;
mod local_mcp_command;
mod local_monitor_command;
mod local_pool_command;
mod local_pool_config_command;
mod local_pool_doctor_command;
mod local_pool_run_command;
mod local_pool_scale_command;
mod local_pool_work_command;
mod local_project_command;
mod local_pump_command;
mod local_pump_record_command;
mod local_report_command;
mod local_role_command;
mod local_rule_command;
mod local_skills_command;
mod local_step_command;
mod local_sync_command;
mod local_want_show;
mod local_work_command;
mod local_work_list;
mod quiet_pipe;
mod remote_mode;

use local_mailspace_command::run_mailspace_command;

fn main() {
    quiet_pipe::install();

    // The server must start single-threaded (it hardens the process before
    // building its own runtime), so it is dispatched before vivi's runtime
    // exists: by name
    // (a `vivi-server` link to this binary) or as `vivi server run`.
    let mut argv = std::env::args();
    let invoked_as = argv.next().unwrap_or_default();
    if std::path::Path::new(&invoked_as)
        .file_name()
        .and_then(|n| n.to_str())
        == Some("vivi-server")
    {
        vivi_server::server::entry::run(argv.collect());
        return;
    }

    let Some(cli) = run_before_runtime(Cli::parse()) else {
        return;
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            eprintln!("error: cannot start the async runtime: {e}");
            process::exit(1);
        });
    runtime.block_on(async_main(cli));
}

/// The commands that need no async runtime. `None` when one ran; the parsed
/// command line back when it still needs the runtime.
fn run_before_runtime(cli: Cli) -> Option<Cli> {
    match &cli.command {
        Command::Server {
            command: ServerSubcommand::Run { args },
        } => vivi_server::server::entry::run(args.clone()),
        Command::Pool {
            command: vivi_core::cli::PoolSubcommand::Run(run),
        } => process::exit(local_pool_run_command::run(run)),
        Command::Key {
            command: vivi_core::cli::KeyCommand::Show,
        } => key_show(),
        Command::Key {
            command: vivi_core::cli::KeyCommand::GitCredential { action },
        } => git_credential(action),
        Command::Server {
            command: ServerSubcommand::Pump { action },
        } => server_pump(action),
        Command::Server {
            command: ServerSubcommand::Init { port },
        } => server_init(*port),
        _ => return Some(cli),
    }
    None
}

/// `vivi key show`: this machine's public key, fingerprint and key file.
fn key_show() {
    match vivi_core::machine_key::MachineKey::load_or_create() {
        Ok(key) => {
            println!("public_key  {}", key.public_key());
            println!("fingerprint {}", key.fingerprint());
            println!("file        {}", key.path().display());
        }
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}

/// `vivi server pump <pause|resume|status>`.
fn server_pump(action: &vivi_core::cli::ServerPumpAction) {
    let file = match vivi_util::home::server_home() {
        Ok(home) => home.join("pump-state"),
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };
    let write = |state: &str| {
        if let Err(e) = std::fs::write(&file, format!("{state}\n")) {
            eprintln!("error: cannot write {}: {e}", file.display());
            process::exit(1);
        }
    };
    match action {
        vivi_core::cli::ServerPumpAction::Pause => write("paused"),
        vivi_core::cli::ServerPumpAction::Resume => write("running"),
        vivi_core::cli::ServerPumpAction::Status => {}
    }
    let paused = vivi_core::pump::service::server_pump_paused(&file);
    println!("server pump {}", if paused { "paused" } else { "running" });
}

/// `vivi server init`.
fn server_init(port: Option<u16>) {
    if let Err(e) = vivi_server::server::init::run(port) {
        eprintln!("error: {e}");
        process::exit(1);
    }
}

async fn async_main(cli: Cli) {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            if cli.verbose {
                EnvFilter::new("vivi=debug")
            } else {
                EnvFilter::new("vivi=info")
            }
        }))
        .init();

    if let Err(e) = run(cli).await {
        eprintln!("error: {e}");
        process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), VivariumError> {
    if matches!(
        &cli.command,
        Command::Pump {
            command: PumpSubcommand::Service {
                command: PumpServiceCommand::Run
            }
        }
    ) {
        return vivi_core::pump::service::run().await;
    }
    if let Command::Proxy {
        command: ProxySubcommand::Run(cmd),
    } = &cli.command
    {
        return vivi_proxy::proxy::run(cmd).await;
    }
    if let Command::Mind(command) = &cli.command {
        return run_mind(command).await;
    }
    if let Command::Doctor(command) = &cli.command {
        // Doctor probes use reqwest's blocking client, which builds and drops
        // its own runtime; that panics on an async worker thread.
        return tokio::task::block_in_place(|| {
            local_doctor_command::run(command, cli.project.as_deref())
        });
    }
    if let Some(result) = dispatch_self_update(&cli.command) {
        return result;
    }
    run_mailspace_command(&cli.command)
}

#[cfg(unix)]
async fn run_mind(command: &vivi_core::cli::MindCommand) -> Result<(), VivariumError> {
    vivi_mind::mind::run(command).await
}

#[cfg(not(unix))]
async fn run_mind(_command: &vivi_core::cli::MindCommand) -> Result<(), VivariumError> {
    Err(VivariumError::Other("vivi mind needs a Unix host".into()))
}

fn dispatch_self_update(command: &Command) -> Option<Result<(), VivariumError>> {
    match command {
        Command::Update(cmd) if cmd.check => Some(run_update_check()),
        Command::Update(cmd) => Some(vivi_update::update::run_update(cmd)),
        Command::Versions(cmd) => Some(vivi_update::update::print_versions(cmd)),
        Command::Uninstall(cmd) => Some(vivi_update::update::uninstall(cmd)),
        _ => None,
    }
}

fn run_update_check() -> Result<(), VivariumError> {
    match vivi_update::update::check_for_update() {
        Ok(CheckOutcome::Available) => process::exit(1),
        Ok(_) => Ok(()),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(2);
        }
    }
}

/// `vivi key git-credential <get|store|erase>`: git's credential-helper
/// protocol over this machine's key.
fn git_credential(action: &str) {
    use std::io::Read;
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("error: cannot read git's credential request: {e}");
        process::exit(1);
    }
    let field = |name: &str| {
        input
            .lines()
            .find_map(|line| {
                line.strip_prefix(name)
                    .and_then(|rest| rest.strip_prefix('='))
            })
            .unwrap_or_default()
            .to_owned()
    };
    let (protocol, host) = (field("protocol"), field("host"));
    if protocol.is_empty() || host.is_empty() {
        return;
    }
    let server = format!("{protocol}://{host}");
    match action {
        // Always a fresh sign-in: git does not retry after a 401, so a cached
        // token a restarted server no longer knows would fail the command.
        "get" => match vivi_core::machine_key::forget(&server, false)
            .and_then(|()| vivi_core::machine_key::sign_in(&server, false))
        {
            Ok(token) => {
                println!("username=vivi");
                println!("password={token}");
            }
            Err(e) => {
                eprintln!("error: {e}");
                process::exit(1);
            }
        },
        "erase" => {
            if let Err(e) = vivi_core::machine_key::forget(&server, false) {
                eprintln!("warning: cannot forget the cached session: {e}");
            }
        }
        _ => {}
    }
}
