use std::path::{Path, PathBuf};

use chrono::Duration;
use vivi::VivariumError;
use vivi::cli::{
    ReportBy, ReportCommand, ReportCommon, ReportFormat, ReportRecordsFormat, ReportSubcommand,
};
use vivi::report::format::OutputFormat;
use vivi::report::model::{Dimension, Filter, View};

pub(crate) fn run(command: &ReportCommand) -> Result<(), VivariumError> {
    let (view, common, format) = match &command.command {
        ReportSubcommand::Records(command) => (
            View::Records,
            &command.common,
            records_format(command.format),
        ),
        ReportSubcommand::Throughput(command) => (
            View::Throughput,
            &command.common,
            view_format(command.format),
        ),
        ReportSubcommand::Durations(command) => (
            View::Durations,
            &command.common,
            view_format(command.format),
        ),
        ReportSubcommand::Needs(command) => {
            (View::Needs, &command.common, view_format(command.format))
        }
        ReportSubcommand::Failures(command) => {
            (View::Failures, &command.common, view_format(command.format))
        }
    };
    let root = project_root(common.project.as_deref())?;
    let filter = filter(common)?;
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

fn filter(common: &ReportCommon) -> Result<Filter, VivariumError> {
    Ok(Filter {
        since: parse_bound(common.since.as_deref(), false)?,
        until: parse_bound(common.until.as_deref(), true)?,
        by: common.by.map(dimension),
        need: common.need.clone(),
        actor: common.actor.clone(),
        model: common.model.clone(),
        top: common.top,
    })
}

fn parse_bound(value: Option<&str>, until: bool) -> Result<Option<u64>, VivariumError> {
    let Some(value) = value else { return Ok(None) };
    let mut parsed = vivi::mailspace::parse_time_bound(value)?;
    if until && is_calendar_date(value) {
        parsed += Duration::days(1) - Duration::seconds(1);
    }
    u64::try_from(parsed.timestamp()).map(Some).map_err(|_| {
        VivariumError::Message(format!(
            "time bound must be at or after the Unix epoch: {value}"
        ))
    })
}

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
