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
        /// Send as a work item that files into the recipient's
        /// needs/tasks/wants (default is plain chat)
        #[arg(long, value_parser = ["need", "task", "want"])]
        kind: Option<String>,
        /// Item this answers: a local handle or a full msg_ id
        #[arg(long)]
        reply_to: Option<String>,
        /// Item this cites (repeatable): a local handle or a full msg_ id
        #[arg(long = "ref")]
        refs: Vec<String>,
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
    /// Long-poll joined rooms and print arrivals live (Ctrl-C to exit)
    Watch {
        /// Room id (defaults to every joined room)
        room_id: Option<String>,
        /// Exit after the first poll cycle
        #[arg(long)]
        once: bool,
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Pull room items into the mailspace: needs, tasks, and wants file by
    /// kind under their traveling ids (requires [chat] recipient)
    Pull {
        /// Room id (defaults to every joined room)
        room_id: Option<String>,
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Leave a joined room (local unjoin; the server seat stays)
    Leave {
        /// Room id (5 groups of Crockford base32)
        room_id: String,
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Room lifecycle (create)
    Room {
        #[command(subcommand)]
        command: RoomSubcommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum RoomSubcommand {
    /// Create a room on the chat server; the creator auto-joins seat 1
    Create {
        /// Seat capacity 2..8 (server default 4)
        #[arg(long)]
        seats: Option<u32>,
        /// Creator seat label (default <short-host>.<project>, or [chat] name)
        #[arg(long)]
        label: Option<String>,
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
}
