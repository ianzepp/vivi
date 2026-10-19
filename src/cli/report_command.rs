//! Fixed report views and raw attempt-record export.

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Args)]
pub struct ReportCommand {
    #[command(subcommand)]
    pub command: ReportSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ReportSubcommand {
    /// Export one row per attempt as CSV or JSON Lines
    Records(ReportRecordsCommand),
    /// Count dispatched, delivered, merged and failed work by time bucket or dimension
    Throughput(ReportViewCommand),
    /// Summarize wait-while-ready, agent and delivered-to-merged durations
    Durations(ReportViewCommand),
    /// Summarize lead time, effort and attempt count per need
    Needs(ReportViewCommand),
    /// Count failure reasons and repeat-failing units
    Failures(ReportViewCommand),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ReportBy {
    Actor,
    Role,
    Model,
    Pool,
    Kind,
    Need,
    Day,
    Hour,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ReportFormat {
    Table,
    Json,
    Csv,
    Md,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ReportRecordsFormat {
    Csv,
    Jsonl,
}

#[derive(Debug, Args)]
pub struct ReportCommon {
    /// Include records at or after this age, date, or RFC 3339 time
    #[arg(long, value_name = "SINCE")]
    pub since: Option<String>,
    /// Include records at or before this age, date, or RFC 3339 time
    #[arg(long, value_name = "UNTIL")]
    pub until: Option<String>,
    /// Keep only one need
    #[arg(long)]
    pub need: Option<String>,
    /// Keep only one actor
    #[arg(long)]
    pub actor: Option<String>,
    /// Keep only one model
    #[arg(long)]
    pub model: Option<String>,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ReportRecordsCommand {
    #[command(flatten)]
    pub common: ReportCommon,
    /// Raw record format
    #[arg(long, value_enum)]
    pub format: ReportRecordsFormat,
}

#[derive(Debug, Args)]
pub struct ReportViewCommand {
    #[command(flatten)]
    pub common: ReportCommon,
    /// Group by one fixed dimension
    #[arg(long, value_enum)]
    pub by: Option<ReportBy>,
    /// Show at most this many rows
    #[arg(long)]
    pub top: Option<usize>,
    /// Output format
    #[arg(long, value_enum, default_value = "table")]
    pub format: ReportFormat,
}
