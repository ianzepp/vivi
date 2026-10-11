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
        /// Start a mailspace with no pull cursor at a window: an age (7d, 12h,
        /// 90m), an RFC 3339 time, or a YYYY-MM-DD date (midnight UTC). The
        /// history floor is recorded; refused once a cursor exists
        #[arg(long, value_name = "AGE|DATE")]
        since: Option<String>,
        /// After the pull, look up the letters the new letters reference
        /// (X-Vivi-Depends-On, 8-hex handles in the subject or body)
        #[arg(long)]
        refs: bool,
        /// Hops of references to follow, 1 to 3
        #[arg(long, default_value_t = 1, requires = "refs", value_name = "N")]
        depth: u8,
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Reset the pull cursor and history floor to zero and pull everything
    /// again; evicted letters come back byte for byte
    Resync {
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Look letters up by handle (8 hex characters of a message id) and file
    /// them; the cursor and floor are untouched
    Fetch {
        /// Handles to fetch
        #[arg(required = true, value_name = "HANDLE")]
        handles: Vec<String>,
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Evict old letters bound to the project from the local store (never
    /// open work, starred, absorbed, or local-origin mail); restore with
    /// resync, fetch, or pull --refs. Needs no server or key
    Prune {
        /// Evict letters dated before now minus this age (30d, 12h, 90m)
        #[arg(long, value_name = "AGE")]
        older_than: String,
        /// Report what would be evicted and change nothing
        #[arg(long)]
        dry_run: bool,
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
