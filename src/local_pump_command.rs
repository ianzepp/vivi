//! `vivi pump` dispatch: `run`, `once`, and `status` over the run module.

use std::path::{Path, PathBuf};

use vivi::VivariumError;
use vivi::cli::PumpSubcommand;
use vivi::pump::run as pump_run;

pub(crate) fn handle_pump_command(command: &PumpSubcommand) -> Result<(), VivariumError> {
    match command {
        PumpSubcommand::Run(cmd) => pump_run::run(&project_root(&cmd.project), false),
        PumpSubcommand::Once(cmd) => pump_run::run(&project_root(&cmd.project), true),
        PumpSubcommand::Status(cmd) => {
            let text = pump_run::status(&project_root(&cmd.project))?;
            print!("{text}");
            Ok(())
        }
    }
}

/// The project root: the subcommand's `--project`, else the current
/// directory (whose ancestors `Mailspace::discover` walks).
fn project_root(project: &Option<PathBuf>) -> PathBuf {
    project
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}
