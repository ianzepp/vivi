#![deny(clippy::pedantic)]

//! The `vivi` binary.

use std::process;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use vivi::VivariumError;
use vivi::cli::{Cli, Command, ProxySubcommand, PumpServiceCommand, PumpSubcommand};
use vivi::update::CheckOutcome;

mod local_board_command;
mod local_boot_command;
mod local_config_command;
mod local_delivery_command;
mod local_doctor_command;
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
mod local_work_command;
mod local_work_list;
mod quiet_pipe;

use local_mailspace_command::run_mailspace_command;

#[tokio::main]
async fn main() {
    quiet_pipe::install();

    let cli = Cli::parse();

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
