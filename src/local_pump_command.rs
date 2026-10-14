//! `vivi pump` command handling: scaffolding only, the pump is not built yet.

use vivi::VivariumError;
use vivi::cli::PumpSubcommand;

pub(crate) fn handle_pump_command(command: &PumpSubcommand) -> Result<(), VivariumError> {
    match command {
        PumpSubcommand::Run(_) => Err(VivariumError::Message(
            "vivi pump is not built yet".to_string(),
        )),
        PumpSubcommand::Status(_) => Err(VivariumError::Message(
            "vivi pump is not built yet".to_string(),
        )),
        PumpSubcommand::Once(_) => Err(VivariumError::Message(
            "vivi pump is not built yet".to_string(),
        )),
    }
}
