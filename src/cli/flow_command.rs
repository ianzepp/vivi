//! `vivi flow`: list and show the shipped flows, and a need's run index.

use std::path::PathBuf;

use clap::Subcommand;

/// Inspect the flows compiled into this binary. Read-only.
#[derive(Debug, Subcommand)]
pub enum FlowCommand {
    /// List the shipped flow kinds
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Print a shipped flow's Mermaid source, and with `--need` that need's runs
    Show {
        /// Flow kind: build, research or release
        kind: String,

        /// Append the run index of this need handle (steps with task, state, verdict)
        #[arg(long)]
        need: Option<String>,

        /// Project root that owns .vivi/ (also accepted globally)
        #[arg(long)]
        project: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}
