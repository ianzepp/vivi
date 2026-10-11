#![deny(clippy::pedantic)]

//! The `vivi` binary.

use std::process;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use vivi::VivariumError;
use vivi::cli::Cli;

mod local_board_command;
mod local_boot_command;
mod local_goal_command;
mod local_graph_command;
mod local_mail_list;
mod local_mailspace_command;
mod local_mailspace_dump;
mod local_pool_command;
mod local_role_command;
mod local_rule_command;
mod local_step_command;
mod local_work_command;
mod local_work_list;

use local_mailspace_command::run_mailspace_command;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
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
    run_mailspace_command(&cli.command)
}
