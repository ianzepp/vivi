use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use vivi::VivariumError;
use vivi::cli::{
    ReportBy, ReportCommand, ReportCommon, ReportFormat, ReportRecordsFormat, ReportSubcommand,
};
use vivi::report::format::OutputFormat;
use vivi::report::model::{Dimension, Filter, View};

pub(crate) fn run(command: &ReportCommand) -> Result<(), VivariumError> {
    let (view, common, format, by, top) = match &command.command {
        ReportSubcommand::Records(command) => (
            View::Records,
            &command.common,
            records_format(command.format),
            None,
            None,
        ),
        ReportSubcommand::Throughput(command) => (
            View::Throughput,
            &command.common,
            view_format(command.format),
            command.by,
            command.top,
        ),
        ReportSubcommand::Durations(command) => (
            View::Durations,
            &command.common,
            view_format(command.format),
            command.by,
            command.top,
        ),
        ReportSubcommand::Needs(command) => (
            View::Needs,
            &command.common,
            view_format(command.format),
            command.by,
            command.top,
        ),
        ReportSubcommand::Failures(command) => (
            View::Failures,
            &command.common,
            view_format(command.format),
            command.by,
            command.top,
        ),
    };
    let root = project_root(common.project.as_deref())?;
    let filter = filter(common, by, top)?;
    let data = vivi::report::data::read(&root)?;
    let rows = vivi::report::views::rows(&data, view, &filter);
    let rendered = vivi::report::format::render(&rows, vivi::report::views::columns(view), format)?;
    print!("{rendered}");
    Ok(())
}

fn view_format(format: ReportFormat) -> OutputFormat {
    match format {
        ReportFormat::Table => OutputFormat::Table,
        ReportFormat::Json => OutputFormat::Json,
        ReportFormat::Csv => OutputFormat::Csv,
        ReportFormat::Md => OutputFormat::Markdown,
    }
}

fn records_format(format: ReportRecordsFormat) -> OutputFormat {
    match format {
        ReportRecordsFormat::Csv => OutputFormat::Csv,
        ReportRecordsFormat::Jsonl => OutputFormat::JsonLines,
    }
}

fn filter(
    common: &ReportCommon,
    by: Option<ReportBy>,
    top: Option<usize>,
) -> Result<Filter, VivariumError> {
    Ok(Filter {
        since: parse_bound(common.since.as_deref(), false)?,
        until: parse_bound(common.until.as_deref(), true)?,
        by: by.map(dimension),
        need: common.need.clone(),
        actor: common.actor.clone(),
        model: common.model.clone(),
        top,
    })
}

fn parse_bound(value: Option<&str>, until: bool) -> Result<Option<u64>, VivariumError> {
    let Some(value) = value else { return Ok(None) };
    let timestamp = if is_calendar_date(value) {
        let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map_err(|_| VivariumError::Message(format!("invalid calendar date '{value}'")))?;
        let bound_date = if until {
            date.succ_opt().ok_or_else(|| {
                VivariumError::Message(format!("calendar date '{value}' has no following day"))
            })?
        } else {
            date
        };
        let midnight = bound_date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| VivariumError::Message(format!("invalid calendar date '{value}'")))?
            .and_utc()
            .timestamp();
        if until { midnight - 1 } else { midnight }
    } else {
        vivi::mailspace::parse_time_bound(value)?.timestamp()
    };
    u64::try_from(timestamp).map(Some).map_err(|_| {
        VivariumError::Message(format!(
            "time bound must be at or after the Unix epoch: {value}"
        ))
    })
}

#[cfg(test)]
#[path = "local_report_command_test.rs"]
mod tests;

fn is_calendar_date(value: &str) -> bool {
    value.len() == 10 && value.as_bytes()[4] == b'-' && value.as_bytes()[7] == b'-'
}

fn dimension(by: ReportBy) -> Dimension {
    match by {
        ReportBy::Actor => Dimension::Actor,
        ReportBy::Role => Dimension::Role,
        ReportBy::Model => Dimension::Model,
        ReportBy::Pool => Dimension::Pool,
        ReportBy::Kind => Dimension::Kind,
        ReportBy::Need => Dimension::Need,
        ReportBy::Day => Dimension::Day,
        ReportBy::Hour => Dimension::Hour,
    }
}

fn project_root(explicit: Option<&Path>) -> Result<PathBuf, VivariumError> {
    if let Some(path) = explicit {
        return Ok(path.to_owned());
    }
    let cwd = std::env::current_dir()?;
    cwd.ancestors()
        .find(|path| path.join(".vivi/mailspace.toml").is_file())
        .map(Path::to_owned)
        .ok_or_else(|| {
            VivariumError::Config("no .vivi/mailspace.toml found; pass --project <root>".into())
        })
}
