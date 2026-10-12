//! `vivi skills`: list, show, and install the skills embedded in this binary.

use std::path::PathBuf;

use clap::Subcommand;

/// Agent skills carried in this binary. Their text matches `vivi --version`.
/// No mailspace is required.
#[derive(Debug, Subcommand)]
pub enum SkillsCommand {
    /// List embedded skills (name, one-line description, version)
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Print SKILL.md, or one supporting file, and list the files an agent can request
    Show {
        /// Skill name (`vivi`, `tugboat`, `speedboat`)
        name: String,

        /// Path relative to the skill root. Default: `SKILL.md`
        #[arg(long)]
        file: Option<String>,
    },

    /// Write every embedded skill into a directory, keeping relative links
    Install {
        /// Destination directory (creates `vivi/`, `tugboat/`, `speedboat/`)
        dir: PathBuf,

        /// Overwrite files that already exist
        #[arg(long)]
        force: bool,
    },
}
