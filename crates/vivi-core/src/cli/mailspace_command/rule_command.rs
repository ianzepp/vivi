//! `vivi rule`: project rules shared by every seat.

use std::path::PathBuf;

use clap::{ArgGroup, Args, Subcommand};

use super::AbsorbCommand;

/// Save a project rule. The message is written once.
#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("rule_save_body")
        .required(true)
        .args(["body", "body_file"])
))]
pub struct RuleSaveCommand {
    /// Identity recording the rule
    #[arg(long = "for")]
    pub for_identity: String,

    /// Rule subject
    #[arg(long)]
    pub subject: String,

    /// Rule body
    #[arg(long)]
    pub body: Option<String>,

    /// Read the rule body from a file
    #[arg(long)]
    pub body_file: Option<PathBuf>,

    /// Mark the rule local to this mailspace
    #[arg(long)]
    pub local: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Replace an active rule with a new one.
#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("rule_supersede_body")
        .required(true)
        .args(["body", "body_file"])
))]
pub struct RuleSupersedeCommand {
    /// Rule handle or unambiguous prefix
    pub handle: String,

    /// Identity recording the replacement
    #[arg(long = "for")]
    pub for_identity: String,

    /// Subject of the new rule
    #[arg(long)]
    pub subject: String,

    /// Body of the new rule
    #[arg(long)]
    pub body: Option<String>,

    /// Read the new body from a file
    #[arg(long)]
    pub body_file: Option<PathBuf>,

    /// Mark the new rule local to this mailspace
    #[arg(long)]
    pub local: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Withdraw a rule, or seal one.
#[derive(Debug, Clone, Args)]
pub struct RuleRetireCommand {
    /// Rule handle or unambiguous prefix
    pub handle: String,

    /// Identity acting on the rule
    #[arg(long = "for")]
    pub for_identity: String,

    /// Note recorded on the event
    #[arg(long)]
    pub note: Option<String>,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Project rules. The active set is visible to every seat.
#[derive(Debug, Clone, Subcommand)]
pub enum RuleCommand {
    /// Save a project rule
    Save(RuleSaveCommand),

    /// List rules. The default is the active set.
    List {
        /// Keep rules written by this identity
        #[arg(long = "for")]
        for_identity: Option<String>,

        /// Include retired, superseded, and absorbed rules
        #[arg(long)]
        all: bool,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Show one rule, including a rule that has left the active set
    Show {
        /// Rule handle or unambiguous prefix
        handle: String,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Search rule subjects and bodies
    Search {
        /// Search query (case-insensitive substring)
        query: String,

        /// Keep rules written by this identity
        #[arg(long = "for")]
        for_identity: Option<String>,

        /// Search subject only
        #[arg(long)]
        subject: bool,

        /// Include retired, superseded, and absorbed rules
        #[arg(long)]
        all: bool,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Replace an active rule. The old rule stays readable.
    Supersede(RuleSupersedeCommand),

    /// Withdraw an active rule
    Retire(RuleRetireCommand),

    /// Seal a rule. Once absorbed, it can no longer be changed.
    Absorb(AbsorbCommand),
}
