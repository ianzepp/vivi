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
    /// Join a chat room: challenge -> sign -> seat binding
    Join {
        /// Room id (5 groups of Crockford base32)
        room_id: String,
        /// Seat label (default <short-host>/<project>, or [chat] name)
        #[arg(long)]
        label: Option<String>,
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// List rooms joined from this mailspace
    Rooms {
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
}
