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
    Send(ChatSendCommand),
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
    /// Rebuild the mailspace replica from the room: replay from cursor zero
    /// through the same ingest as pull, restoring pruned items byte-identically
    /// and leaving local state (closed needs, read flags) untouched
    Resync {
        /// Room id (defaults to every joined room)
        room_id: Option<String>,
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Evict old room items from the local cache; resync restores them. Never
    /// touches open needs/tasks/wants, starred or absorbed items, or
    /// local-origin mail
    Prune {
        /// Room id (default: every room)
        room_id: Option<String>,
        /// Evict items dated before this age ago, such as 30d, 12h, or 90m
        #[arg(long)]
        older_than: String,
        /// Show what would be evicted without changing anything
        #[arg(long)]
        dry_run: bool,
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

/// Arguments of `vivi chat send`.
#[derive(Debug, Args)]
pub struct ChatSendCommand {
    /// Room id (5 groups of Crockford base32)
    pub room_id: String,
    /// Direct recipient seat label (omit to broadcast)
    #[arg(long)]
    pub to: Option<String>,
    /// Subject line
    #[arg(long)]
    pub subject: Option<String>,
    /// Message body text
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Read the message body from this file
    #[arg(long)]
    pub body_file: Option<PathBuf>,
    /// Send as a work item that files into the recipient's
    /// needs/tasks/wants (default is plain chat)
    #[arg(long, value_parser = ["need", "task", "want"])]
    pub kind: Option<String>,
    /// Item this answers: a local handle or a full msg_ id
    #[arg(long)]
    pub reply_to: Option<String>,
    /// Item this cites (repeatable): a local handle or a full msg_ id
    #[arg(long = "ref")]
    pub refs: Vec<String>,
    /// Project root that owns .vivi/
    #[arg(long)]
    pub project: Option<PathBuf>,
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
