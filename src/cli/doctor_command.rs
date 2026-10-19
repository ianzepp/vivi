use std::path::PathBuf;

use clap::Args;

#[derive(Debug, Args)]
pub struct DoctorCommand {
    /// Project root that owns .vivi/ (also accepted globally: vivi --project <ROOT> doctor)
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Print the report as JSON
    #[arg(long)]
    pub json: bool,

    /// Include a fix command for each warning or failure
    #[arg(long)]
    pub fix_hints: bool,
}
