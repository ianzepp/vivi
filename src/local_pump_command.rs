//! `vivi pump` dispatch: `run`, `once`, and `status` over the run module.

use std::path::PathBuf;

use vivi::VivariumError;
use vivi::cli::{PumpServiceCommand, PumpSubcommand};
use vivi::pump::control::PumpMode;
use vivi::pump::run as pump_run;

pub(crate) fn handle_pump_command(command: &PumpSubcommand) -> Result<(), VivariumError> {
    match command {
        PumpSubcommand::Run(cmd) if cmd.service_child => {
            pump_run::run_service_child(&project_root(&cmd.project))
        }
        PumpSubcommand::Run(cmd) => pump_run::run(&project_root(&cmd.project), false),
        PumpSubcommand::Once(cmd) => pump_run::run(&project_root(&cmd.project), true),
        PumpSubcommand::Status(cmd) => {
            let text = pump_run::status(&project_root(&cmd.project))?;
            print!("{text}");
            Ok(())
        }
        PumpSubcommand::Export(cmd) => pump_run::export(&project_root(&cmd.project), &cmd.json),
        PumpSubcommand::Closeout(cmd) => pump_run::closeout(
            &project_root(&cmd.project),
            cmd.json,
            cmd.close.as_deref(),
            cmd.for_identity.as_deref(),
        ),
        PumpSubcommand::Pause(cmd) => {
            vivi::pump::control::set_project_mode(&project_root(&cmd.project), PumpMode::Paused)
        }
        PumpSubcommand::Resume(cmd) | PumpSubcommand::Start(cmd) => {
            vivi::pump::control::set_project_mode(&project_root(&cmd.project), PumpMode::Running)
        }
        PumpSubcommand::Stop(cmd) => {
            vivi::pump::control::set_project_mode(&project_root(&cmd.project), PumpMode::Stopped)
        }
        PumpSubcommand::Service { command } => match command {
            PumpServiceCommand::Add(cmd) => vivi::pump::service::add(&cmd.project),
            PumpServiceCommand::Remove(cmd) => vivi::pump::service::remove(&cmd.project),
            PumpServiceCommand::List => vivi::pump::service::list(),
            PumpServiceCommand::Install => vivi::pump::service::install(),
            PumpServiceCommand::Uninstall => vivi::pump::service::uninstall(),
            PumpServiceCommand::Status => vivi::pump::service::status(),
            PumpServiceCommand::Run => Err(VivariumError::Other(
                "pump service run requires the async command entry point".into(),
            )),
        },
    }
}

/// The project root: the subcommand's `--project`, else the current
/// directory (whose ancestors `Mailspace::discover` walks).
fn project_root(project: &Option<PathBuf>) -> PathBuf {
    project
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}
