use std::path::PathBuf;

use clap::{ArgGroup, Subcommand};

/// Manage first-class mailspace agent seats (roles).
#[derive(Debug, Subcommand)]
pub enum RoleCommand {
    /// List roles in the project mailspace
    List {
        /// Project root to inspect
        #[arg(long)]
        project: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Show one role, including charter body
    Show {
        /// Role name (local-part)
        name: String,

        /// Project root to inspect
        #[arg(long)]
        project: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Add a role seat to the roster
    Add {
        /// Role name (local-part), e.g. hand-1 or head-ceo
        name: String,

        /// Process class (hand, head, mind, operator, steward, digest, or freeform)
        #[arg(long)]
        kind: Option<String>,

        /// Execution home (subagent, tmux, `vivi_pty`, …)
        #[arg(long)]
        harness: Option<String>,

        /// Inference provider / account lane
        #[arg(long)]
        provider: Option<String>,

        /// Model id
        #[arg(long)]
        model: Option<String>,

        /// Thinking / reasoning effort
        #[arg(long)]
        thinking: Option<String>,

        /// Lifecycle status (active, parked, retired, or freeform)
        #[arg(long)]
        status: Option<String>,

        /// Freeform label slug (repeatable)
        #[arg(long = "label")]
        labels: Vec<String>,

        /// Desired maximum silence between outbound signals (e.g. 15m, 1h)
        #[arg(long)]
        cadence: Option<String>,

        /// Pool this role dispatches to
        #[arg(long)]
        pool: Option<String>,

        /// Override the kind's read-only default
        #[arg(long)]
        read_only: Option<bool>,

        /// Preferred model slugs or bands (repeatable)
        #[arg(long = "models")]
        models: Vec<String>,

        /// Project root to update
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Update scalar fields on one role (partial update)
    Set {
        /// Role name (local-part)
        name: String,

        /// Process class
        #[arg(long)]
        kind: Option<String>,

        /// Clear kind
        #[arg(long)]
        clear_kind: bool,

        /// Execution home
        #[arg(long)]
        harness: Option<String>,

        /// Clear harness
        #[arg(long)]
        clear_harness: bool,

        /// Inference provider
        #[arg(long)]
        provider: Option<String>,

        /// Clear provider
        #[arg(long)]
        clear_provider: bool,

        /// Model id
        #[arg(long)]
        model: Option<String>,

        /// Clear model
        #[arg(long)]
        clear_model: bool,

        /// Thinking / reasoning effort
        #[arg(long)]
        thinking: Option<String>,

        /// Clear thinking
        #[arg(long)]
        clear_thinking: bool,

        /// Process id of the live process occupying this seat (self-set by the role)
        #[arg(long)]
        pid: Option<u32>,

        /// Clear pid (also clears host, since a binding without a pid has no meaningful host)
        #[arg(long)]
        clear_pid: bool,

        /// Host where the pid lives. Defaults to the local hostname when `--pid` is set.
        #[arg(long)]
        host: Option<String>,

        /// Clear host only (keep pid)
        #[arg(long)]
        clear_host: bool,

        /// Desired maximum silence between outbound signals (e.g. 15m, 1h)
        #[arg(long)]
        cadence: Option<String>,

        /// Clear cadence (role is not on a schedule)
        #[arg(long)]
        clear_cadence: bool,

        /// Lifecycle status
        #[arg(long)]
        status: Option<String>,

        /// Add a freeform label slug (repeatable)
        #[arg(long = "label")]
        labels: Vec<String>,

        /// Remove a label slug (repeatable)
        #[arg(long = "clear-label")]
        clear_labels: Vec<String>,

        /// Pool this role dispatches to
        #[arg(long)]
        pool: Option<String>,

        /// Clear the assigned pool
        #[arg(long)]
        clear_pool: bool,

        /// Override the kind's read-only default
        #[arg(long)]
        read_only: Option<bool>,

        /// Clear the explicit read-only flag
        #[arg(long)]
        clear_read_only: bool,

        /// Preferred model slugs or bands (repeatable)
        #[arg(long = "models")]
        models: Vec<String>,

        /// Clear the models list
        #[arg(long)]
        clear_models: bool,

        /// Project root to update
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Assign a role to a pool (`role set --pool` alias)
    Pool {
        /// Role name (local-part)
        name: String,

        /// Pool name (`[pools.<name>]`)
        pool: String,

        /// Project root to update
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Rename a role; old name is kept as an alias
    Rename {
        /// Current role name
        old: String,

        /// New role name
        new: String,

        /// Project root to update
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Show live process status and schedule health for a role
    Status {
        /// Role name (local-part) or alias
        name: String,

        /// Project root to inspect
        #[arg(long)]
        project: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Show or set the standing charter prompt for a role
    Charter {
        #[command(subcommand)]
        command: RoleCharterCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum RoleCharterCommand {
    /// Print the role charter body
    Show {
        /// Role name
        name: String,

        /// Project root to inspect
        #[arg(long)]
        project: Option<PathBuf>,

        /// Output as JSON object with name + charter
        #[arg(long)]
        json: bool,
    },

    /// Replace the role charter body
    #[command(group(
        ArgGroup::new("charter_body")
            .required(true)
            .args(["body", "body_file", "file"])
    ))]
    Set {
        /// Role name
        name: String,

        /// Charter text, or @path to read from a file
        #[arg(long)]
        body: Option<String>,

        /// Read charter from a file
        #[arg(long)]
        body_file: Option<PathBuf>,

        /// Alias for --body-file (persona/import ergonomics)
        #[arg(long)]
        file: Option<PathBuf>,

        /// Project root to update
        #[arg(long)]
        project: Option<PathBuf>,
    },
}
