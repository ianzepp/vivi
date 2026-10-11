//! CLI surface for the `vivi sync` command family.

use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct SyncCommand {
    #[command(subcommand)]
    pub command: Option<SyncSubcommand>,

    /// Project root that owns .vivi/ (also accepted after a subcommand)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum SyncSubcommand {
    /// Send local letters the server does not have yet (needs, tasks, wants,
    /// and plain mail; never memos, drafts, or trash)
    Push {
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// File letters other members pushed, byte for byte, under the roles
    /// their To and Cc addresses name
    Pull {
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Register [sync] name with the project server (the key must be vouched for)
    Join {
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Show the identity fingerprint, server, project, name, own address
    /// domain, pull cursor, and the count of unsent letters
    Status {
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// List the project's members: name, address domain, and fingerprint
    Directory {
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
}
