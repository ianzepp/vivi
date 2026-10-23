//! `vivi project`: create a project on a host in one operation.

use std::path::PathBuf;

use clap::{Args, Subcommand};

/// Project lifecycle on a host.
#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// Create a project: a group-shared bare git repository, a mailspace with
    /// the starter pump config, the project values, and a host registration.
    /// Any failure undoes what this command created.
    Create(ProjectCreateCommand),
}

#[derive(Debug, Args)]
#[allow(clippy::struct_excessive_bools)]
pub struct ProjectCreateCommand {
    /// Project name: one path segment of lowercase letters, digits, `-`, `.`
    /// and `_`, starting with a letter or digit
    pub name: String,
    /// Directory that holds project roots; the project is `<PROJECTS_ROOT>/<NAME>`
    #[arg(long)]
    pub projects_root: PathBuf,
    /// Directory that holds bare repositories; the repo is `<LANES_ROOT>/<NAME>.git`
    #[arg(long)]
    pub lanes_root: PathBuf,
    /// Git URL prefix; the git URL is `<PREFIX><NAME>.git` (an SSH or file://
    /// address, never http(s))
    #[arg(long)]
    pub git_url_prefix: String,
    /// Pool whose `remote_url` is set to the new git URL (the pool must come
    /// from a base config above the projects root)
    #[arg(long)]
    pub pool: Option<String>,
    /// Integration branch (`[pump] branch`) and the new repo's default branch
    #[arg(long, default_value = "main")]
    pub branch: String,
    /// Register the project without a pump
    #[arg(long)]
    pub no_pump: bool,
    /// Register the project without a server
    #[arg(long)]
    pub no_serve: bool,
    /// Create an empty initial commit on the branch
    #[arg(long)]
    pub initial_commit: bool,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}
