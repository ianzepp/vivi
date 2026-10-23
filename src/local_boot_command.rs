//! `vivi boot` dispatch: collect the frame and print it.

use std::path::Path;

use vivi::VivariumError;
use vivi::boot::BootReport;
use vivi::mailspace::Mailspace;

pub(crate) fn handle_boot_command(project: Option<&Path>, json: bool) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let report = BootReport::collect(&mailspace)?;
    if json {
        let rendered = serde_json::to_string_pretty(&report)
            .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?;
        println!("{rendered}");
        return Ok(());
    }
    print!("{}", report.render());
    Ok(())
}
