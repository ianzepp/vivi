//! CLI surface for the `vivi sync` command family.

use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct SyncCommand {
    #[command(subcommand)]
    pub command: Option<SyncSubcommand>,

    /// Keep the push pass sending through the server's rate limit: wait 61 s
    /// after a 429 and send the rest, until nothing sendable is left
    #[arg(long)]
    pub drain: bool,

    /// Project root that owns .vivi/ (also accepted after a subcommand)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum SyncSubcommand {
    /// Send local letters the server does not have yet (needs, tasks, wants,
    /// and plain mail; never memos, drafts, or trash)
    Push {
        /// Send only letters dated at or after this: an age (7d, 12h, 90m),
        /// an RFC 3339 time, or a YYYY-MM-DD date (midnight UTC). Older
        /// unpushed letters stay unsent until a push without --since
        #[arg(long, value_name = "AGE|DATE")]
        since: Option<String>,
        /// Wait out the server's rate limit: 61 s after each 429, one
        /// progress line per pass, until nothing sendable is left. Still
        /// stops on a 401, a network failure, or another server error
        #[arg(long)]
        drain: bool,
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
    /// Stop pushing a stuck letter (over the 64 KiB cap, or a From that claims
    /// another mailspace); it stays in this mailspace. --undo makes push try
    /// it again
    KeepLocal {
        /// Handles of the letters to keep local (or, with --undo, to try again)
        #[arg(required = true, value_name = "HANDLE")]
        handles: Vec<String>,
        /// Make push try the letters again: lift the kept-local record
        #[arg(long)]
        undo: bool,
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
    /// Print this mailspace's public key as an SPKI PEM (mints the key when
    /// missing); needs no [sync] table, server, or network
    Key {
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Create a project on [sync].server with this mailspace's key as the
    /// creator's, and print the new project id; writes nothing locally —
    /// put the printed id in [sync] yourself, then run vivi sync join
    Create {
        /// The creator name to register (defaults to [sync].name)
        #[arg(long, value_name = "CREATOR")]
        name: Option<String>,
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Mint, list, and revoke PROJECT API keys (a bearer for `vivi mcp` and
    /// the letter wire; not `vivi sync key`, which prints this mailspace's
    /// public key)
    ApiKey {
        #[command(subcommand)]
        command: ApiKeyCommand,
    },
    /// Vouch for a member's key as the admitted member this mailspace is:
    /// sends one governance letter to the project's postmaster (needs
    /// [sync].server, project, and name); stores nothing locally
    Vouch {
        /// The member name the vouch admits
        #[arg(value_name = "NAME")]
        member: String,
        /// File holding the member's public key PEM
        #[arg(
            long,
            value_name = "PATH",
            required_unless_present = "key",
            conflicts_with = "key"
        )]
        key_file: Option<PathBuf>,
        /// The member's public key PEM, given inline
        #[arg(long, value_name = "PEM", required_unless_present = "key_file")]
        key: Option<String>,
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ApiKeyCommand {
    /// Mint a PROJECT API key: a bearer for `vivi mcp` and the letter wire
    /// (not `vivi sync key`, which prints this mailspace's public key). The
    /// raw key is printed once and never stored by vivi
    Mint {
        /// What the key is for, as the project's key list shows it (1 to 64
        /// characters)
        #[arg(long, value_name = "LABEL")]
        label: String,
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// List the project's live API keys: id, label, creator, and time;
    /// revoked keys are not shown and no secret ever is
    List {
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Revoke one PROJECT API key by its key id (the minting member's
    /// identity token does the authorizing; the raw key is not needed)
    Revoke {
        /// The key id to revoke
        #[arg(value_name = "KEY_ID")]
        key_id: String,
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
}
