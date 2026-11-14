//! `vivi pool landed`: record how landing a job's branch ended. The record is
//! [`vivi_core::pool::landed`].

use vivi_core::cli::PoolLandedCommand;
use vivi_core::mailspace::Mailspace;
use vivi_core::pool::landed::{self, LandedRecord, Landing};
use vivi_error::VivariumError;

pub(crate) fn landed(command: &PoolLandedCommand) -> Result<(), VivariumError> {
    let record = LandedRecord {
        job: command.job.clone(),
        landing: Landing::parse(&command.outcome)?,
        base: command.base.clone(),
        tip: command.tip.clone(),
        secs: command.secs,
        command: command.command.clone(),
        tail: tail_of(command)?,
        files: command.files.clone(),
    };
    let space = Mailspace::discover(command.project.as_deref())?;
    landed::record_landed(&space, &command.task, &record)?;
    println!("landing {} {}", record.job, record.landing.word());
    Ok(())
}

/// The text of `--tail-file`, if given.
fn tail_of(command: &PoolLandedCommand) -> Result<Option<String>, VivariumError> {
    let Some(path) = &command.tail_file else {
        return Ok(None);
    };
    let bytes = std::fs::read(path).map_err(|e| {
        VivariumError::Message(format!("cannot read --tail-file {}: {e}", path.display()))
    })?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}
