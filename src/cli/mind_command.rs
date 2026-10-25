//! `vivi mind`: attach to, and supervise, the hub-wide mind service.

use std::path::PathBuf;

use clap::{Args, Subcommand};

/// `vivi mind` with no subcommand attaches to the running mind.
#[derive(Debug, Args)]
pub struct MindCommand {
    #[command(subcommand)]
    pub command: Option<MindSubcommand>,
}

#[derive(Debug, Subcommand)]
pub enum MindSubcommand {
    /// Attach to the running mind's one session (the default)
    Attach,
    /// Show the mind's authority, pause state, last wake, and session
    Status {
        /// Print as JSON
        #[arg(long)]
        json: bool,
    },
    /// Print the last lines of the mind log
    Log {
        /// Lines to print
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
    },
    /// Stop autonomous wakes and mutating actions; attach still works
    Pause,
    /// Resume autonomous wakes
    Resume,
    /// Ask the running mind to exit; the service manager decides whether it restarts
    Stop,
    /// Start the mind (stub: the service manager starts `vivi mind service run`)
    Start,
    /// Compact the mind's session
    Compact,
    /// Show the last messages of the mind's session
    History {
        /// Messages to show
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Supervise the mind service and the projects it watches
    Service {
        #[command(subcommand)]
        command: MindServiceCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum MindServiceCommand {
    /// Watch a registered project again (clears it from the mind's ignore list)
    Add(MindServiceProjectCommand),
    /// Stop watching a registered project (adds it to the mind's ignore list; the registry is untouched)
    Remove(MindServiceProjectCommand),
    /// List registered projects and whether the mind watches each
    List,
    /// Install the mind as an OS service (stub: see `vivi mind service unit`)
    Install,
    /// Remove the OS service (stub)
    Uninstall,
    /// Show whether a mind is listening, and where its files live
    Status,
    /// Run the foreground mind (for an OS service manager)
    Run,
    /// Print an example hardened systemd unit; installs nothing
    Unit,
}

#[derive(Debug, Args)]
pub struct MindServiceProjectCommand {
    /// Registered project root
    pub project: PathBuf,
}
