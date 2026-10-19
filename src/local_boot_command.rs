//! `vivi boot` dispatch: collect the frame and print it.

use std::path::Path;

use vivi::VivariumError;
use vivi::boot::{BootOptions, BootReport, PumpView};
use vivi::mailspace::Mailspace;

pub(crate) fn handle_boot_command(
    project: Option<&Path>,
    role: Option<&str>,
    pump: Option<PumpView>,
    json: bool,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let opts = BootOptions {
        role: role.map(str::to_string),
        pump,
    };
    let report = BootReport::collect_with(&mailspace, &opts)?;
    if json {
        let rendered = serde_json::to_string_pretty(&report)
            .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?;
        println!("{rendered}");
        return Ok(());
    }
    print!("{}", report.render());
    Ok(())
}
