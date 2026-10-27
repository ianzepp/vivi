#![deny(clippy::pedantic)]

//! The `vivi` binary.

use std::process;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use vivi::VivariumError;
use vivi::cli::{
    Cli, Command, ProxySubcommand, PumpServiceCommand, PumpSubcommand, ServerSubcommand,
};
use vivi::update::CheckOutcome;

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

    // The server must start single-threaded (it hardens the process and
    // takes its secrets out of the environment before building its own
    // runtime), so it is dispatched before vivi's runtime exists: by name
    // (a `vivi-server` link to this binary) or as `vivi server run`.
    let mut argv = std::env::args();
    let invoked_as = argv.next().unwrap_or_default();
    if std::path::Path::new(&invoked_as)
        .file_name()
        .and_then(|n| n.to_str())
        == Some("vivi-server")
    {
        vivi::server::entry::run(argv.collect());
        return;
    }

    let cli = Cli::parse();
    if let Command::Server {
        command: ServerSubcommand::Run { args },
    } = cli.command
    {
        vivi::server::entry::run(args);
        return;
    }
    if let Command::Pool {
        command: vivi::cli::PoolSubcommand::Run(run),
    } = &cli.command
    {
        process::exit(local_pool_run_command::run(run));
    }
    if let Command::Key {
        command: vivi::cli::KeyCommand::Show,
    } = cli.command
    {
        match vivi::machine_key::MachineKey::load_or_create() {
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
        return;
    }
    if let Command::Key {
        command: vivi::cli::KeyCommand::GitCredential { action },
    } = &cli.command
    {
        git_credential(action);
        return;
    }
    if let Command::Server {
        command: ServerSubcommand::Pump { action },
    } = cli.command
    {
        let file = match vivi::home::server_home() {
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
            vivi::cli::ServerPumpAction::Pause => write("paused"),
            vivi::cli::ServerPumpAction::Resume => write("running"),
            vivi::cli::ServerPumpAction::Status => {}
        }
        let paused = vivi::pump::service::server_pump_paused(&file);
        println!("server pump {}", if paused { "paused" } else { "running" });
        return;
    }
    if let Command::Server {
        command: ServerSubcommand::Init { port },
    } = cli.command
    {
        if let Err(e) = vivi::server::init::run(port) {
            eprintln!("error: {e}");
            process::exit(1);
        }
        return;
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            eprintln!("error: cannot start the async runtime: {e}");
            process::exit(1);
        });
    runtime.block_on(async_main(cli));
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
        return vivi::pump::service::run().await;
    }
    if let Command::Proxy {
        command: ProxySubcommand::Run(cmd),
    } = &cli.command
    {
        return vivi::proxy::run(cmd).await;
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
async fn run_mind(command: &vivi::cli::MindCommand) -> Result<(), VivariumError> {
    vivi::mind::run(command).await
}

#[cfg(not(unix))]
async fn run_mind(_command: &vivi::cli::MindCommand) -> Result<(), VivariumError> {
    Err(VivariumError::Other("vivi mind needs a Unix host".into()))
}

fn dispatch_self_update(command: &Command) -> Option<Result<(), VivariumError>> {
    match command {
        Command::Update(cmd) if cmd.check => Some(run_update_check()),
        Command::Update(cmd) => Some(vivi::update::run_update(cmd)),
        Command::Versions(cmd) => Some(vivi::update::print_versions(cmd)),
        Command::Uninstall(cmd) => Some(vivi::update::uninstall(cmd)),
        _ => None,
    }
}

fn run_update_check() -> Result<(), VivariumError> {
    match vivi::update::check_for_update() {
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
    let _ = std::io::stdin().read_to_string(&mut input);
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
        "get" => match vivi::machine_key::forget(&server, false)
            .and_then(|()| vivi::machine_key::sign_in(&server, false))
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
            let _ = vivi::machine_key::forget(&server, false);
        }
        _ => {}
    }
}
