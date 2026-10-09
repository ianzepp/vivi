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
    /// Send a message: enqueue in the outbox, then try immediate delivery
    Send {
        /// Room id (5 groups of Crockford base32)
        room_id: String,
        /// Direct recipient seat label (omit to broadcast)
        #[arg(long)]
        to: Option<String>,
        /// Subject line
        #[arg(long)]
        subject: Option<String>,
        /// Message body text
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        /// Read the message body from this file
        #[arg(long)]
        body_file: Option<PathBuf>,
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Deliver pending outbox messages (also runs before every chat command)
    Flush {
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Read messages since the stored cursor, with origin labels
    Read {
        /// Room id (defaults to the only joined room)
        room_id: Option<String>,
        /// Advance the stored read cursor (default is a preview)
        #[arg(long)]
        mark_read: bool,
        /// Re-render every message from cursor zero, without advancing
        #[arg(long)]
        all: bool,
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
}
