//! `vivi project`: create a project on a host in one operation.

use std::path::PathBuf;

use clap::{Args, Subcommand};

/// Project lifecycle on a host.
#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// Create a project: its lanes directory with the first target's
    /// group-shared bare repository, a mailspace with the starter pump config,
    /// the first target's checkout and entry, the project values, and a host
    /// registration. Any failure undoes what this command created.
    Create(ProjectCreateCommand),
    /// Delete a project: its host registration, its project root and its
    /// lanes directory with every lane in it. Waits for no pump: while one
    /// still holds the project's lock it unregisters and asks to be run again.
    Delete(ProjectDeleteCommand),
    /// Lane lifecycle on a host: attach repository lanes to a project.
    Lane {
        #[command(subcommand)]
        command: ProjectLaneCommand,
    },
}

/// Lane lifecycle on a host.
#[derive(Debug, Subcommand)]
pub enum ProjectLaneCommand {
    /// Add a lane to an existing project: the group-shared bare repository
    /// `<LANES_ROOT>/<PROJECT>/<TARGET_ID>.git`, and inside the project a
    /// checkout plus the `[pump.targets.<id>]` entry.
    Add(ProjectLaneAddCommand),
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
    /// Directory that holds each project's lanes directory; the first
    /// target's repo is `<LANES_ROOT>/<NAME>/<TARGET>.git`
    #[arg(long)]
    pub lanes_root: PathBuf,
    /// Git URL prefix; lane URLs are `<PREFIX><NAME>/<TARGET>.git` (an SSH or
    /// file:// address, never http(s))
    #[arg(long)]
    pub git_url_prefix: String,
    /// The first target's id (lowercase letters, digits and hyphens); defaults
    /// to the project name
    #[arg(long)]
    pub target: Option<String>,
    /// Pool whose `remote_url` is set to the first target's git URL (the pool must come
    /// from a base config above the projects root)
    #[arg(long)]
    pub pool: Option<String>,
    /// The first target's default branch, also its main
    #[arg(long, default_value = "main")]
    pub branch: String,
    /// Register the project without a pump
    #[arg(long)]
    pub no_pump: bool,
    /// Register the project without a server
    #[arg(long)]
    pub no_serve: bool,
    /// Create an empty initial commit on the branch
    #[arg(long, conflicts_with = "source")]
    pub initial_commit: bool,
    /// Fill the first target from this Git address (every branch and tag),
    /// fetched by this host; `--branch` must be one of its branches
    #[arg(long)]
    pub source: Option<String>,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ProjectLaneAddCommand {
    /// Project root that owns .vivi/
    pub project_root: PathBuf,
    /// Target id: the `[pump.targets.<id>]` key; the lane is
    /// `<LANES_ROOT>/<PROJECT>/<TARGET_ID>.git` (`<PROJECT>` is the project
    /// root's directory name) and the checkout is `<PROJECT_ROOT>/<TARGET_ID>`
    pub target_id: String,
    /// Directory that holds each project's lanes directory
    #[arg(long)]
    pub lanes_root: PathBuf,
    /// Git URL prefix; the lane URL is `<PREFIX><PROJECT>/<TARGET_ID>.git` (an SSH or
    /// file:// address, never http(s)). Without it the lane is cloned from
    /// its local path and the target writes no remote_url
    #[arg(long)]
    pub git_url_prefix: Option<String>,
    /// Create the seed commit on the lane's branch
    #[arg(long, conflicts_with = "source")]
    pub seed: bool,
    /// Fill the lane from this Git address (every branch and tag), fetched by
    /// this host; it must have a `main` branch
    #[arg(long)]
    pub source: Option<String>,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ProjectDeleteCommand {
    /// Project name, as given to `vivi project create`
    pub name: String,
    /// Directory that holds project roots
    #[arg(long)]
    pub projects_root: PathBuf,
    /// Directory that holds each project's lanes directory
    #[arg(long)]
    pub lanes_root: PathBuf,
    /// The project name again: this removes its mailspace and every lane's
    /// history on this host
    #[arg(long)]
    pub confirm: String,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}
