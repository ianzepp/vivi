//! CLI surface for the `vivi chat` command family.

use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct ChatCommand {
    #[command(subcommand)]
    pub command: ChatSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ChatSubcommand {
    /// Show chat identity fingerprint, server, joined rooms, and outbox depth
    Status {
        /// Project root that owns .vivi/ (also accepted globally: vivi --project <ROOT> chat status)
        #[arg(long)]
        project: Option<PathBuf>,
    },
}
