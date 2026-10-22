//! `vivi pump send|list|show|done|reopen|history`: the pump record verbs.

use vivi::VivariumError;
use vivi::cli::{
    PumpDoneCommand, PumpHistoryCommand, PumpListCommand, PumpListStatus, PumpReopenCommand,
    PumpSendCommand, PumpShowCommand,
};
use vivi::mailspace::{Mailspace, PumpRecord, PumpState, read_body_arg};

pub(crate) fn send(cmd: &PumpSendCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(cmd.project.as_deref())?;
    let body = cmd.body.as_deref().map(read_body_arg).transpose()?;
    let record = mailspace.pump_send(cmd.from.as_deref(), &cmd.need, body.as_deref())?;
    println!("pump record {} for need {}", record.handle, record.need);
    Ok(())
}

pub(crate) fn list(cmd: &PumpListCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(cmd.project.as_deref())?;
    let state = match cmd.status {
        PumpListStatus::Open => Some(PumpState::Open),
        PumpListStatus::Done => Some(PumpState::Done),
        PumpListStatus::All => None,
    };
    let records = mailspace.pump_list(cmd.need.as_deref(), state)?;
    print_records(&records, cmd.json, "no pump records")
}

pub(crate) fn history(cmd: &PumpHistoryCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(cmd.project.as_deref())?;
    let records = mailspace.pump_history(&cmd.need)?;
    print_records(&records, cmd.json, "no pump records for this need")
}

pub(crate) fn show(cmd: &PumpShowCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(cmd.project.as_deref())?;
    let (record, body) = mailspace.pump_show(&cmd.handle)?;
    if cmd.json {
        let mut value = serde_json::to_value(&record).map_err(encode_error)?;
        value["body"] = serde_json::Value::String(body);
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(encode_error)?
        );
        return Ok(());
    }
    println!("handle   {}", record.handle);
    println!("need     {}", record.need);
    println!("status   {}", record.status.as_str());
    println!("from     {}", record.from);
    println!("to       {}", record.to);
    println!("date     {}", record.date);
    println!("subject  {}", record.subject);
    if let Some(outcome) = &record.outcome {
        println!("outcome  {outcome}");
    }
    println!();
    println!("{}", body.trim_end());
    Ok(())
}

pub(crate) fn done(cmd: &PumpDoneCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(cmd.project.as_deref())?;
    let record = mailspace.pump_done(&cmd.handle, cmd.note.as_deref())?;
    match &record.outcome {
        Some(outcome) => println!("done {} ({outcome})", record.handle),
        None => println!("done {}", record.handle),
    }
    Ok(())
}

pub(crate) fn reopen(cmd: &PumpReopenCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(cmd.project.as_deref())?;
    let record = mailspace.pump_reopen(&cmd.handle)?;
    println!("reopened {}", record.handle);
    Ok(())
}

fn print_records(records: &[PumpRecord], json: bool, empty: &str) -> Result<(), VivariumError> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(records).map_err(encode_error)?
        );
        return Ok(());
    }
    if records.is_empty() {
        println!("  {empty}");
        return Ok(());
    }
    println!("  handle  need  status  date  outcome");
    for record in records {
        println!(
            "  {}  {}  {}  {}  {}",
            record.handle,
            record.need,
            record.status.as_str(),
            record.date,
            record.outcome.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

fn encode_error(error: serde_json::Error) -> VivariumError {
    VivariumError::Other(format!("failed to encode JSON: {error}"))
}
