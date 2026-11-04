//! `vivi config`: show, validate, migrate, set, and unset mailspace config.

use std::path::PathBuf;

use clap::Subcommand;

/// Inspect or migrate the mailspace routing table.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Table of `role -> kind -> pool -> slots -> models -> read_only`
    Show {
        /// Include live free/total slot counts
        #[arg(long)]
        resolved: bool,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Print only result lines in a fixed format
        #[arg(long, conflicts_with = "json")]
        porcelain: bool,

        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Check roles, pools and pump together. Exit 0 when ready, 1 when
    /// invalid, 3 when valid but not ready (items the pump needs are missing)
    Validate {
        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Convert old-style pump routing into role.pool assignments
    Migrate {
        /// Write the converted file (default is a dry-run diff)
        #[arg(long)]
        write: bool,

        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Set one key of this project's mailspace.toml (a dotted TOML key; the
    /// value is parsed as TOML, a bare word is a string); refused while the
    /// pump is running
    Set {
        /// Dotted key, e.g. pump.tick
        key: String,

        /// New value, parsed as TOML; a bare word is a string
        #[arg(allow_hyphen_values = true)]
        value: String,

        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Remove one key from this project's mailspace.toml; refused while the
    /// pump is running
    Unset {
        /// Dotted key, e.g. pump.tick
        key: String,

        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
}
