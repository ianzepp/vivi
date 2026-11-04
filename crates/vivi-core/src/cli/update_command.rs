//! `vivi update`, `vivi versions`, and `vivi uninstall`.

use std::path::PathBuf;

use clap::Args;

/// Install or switch a public release binary.
#[derive(Debug, Args)]
pub struct UpdateCommand {
    /// Report whether a newer public release exists; do not install
    #[arg(long, conflicts_with = "version")]
    pub check: bool,

    /// Install this exact version (downgrade and rollback are allowed)
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,

    /// Release asset base URL or directory (default: GitHub releases)
    #[arg(long = "base-url", value_name = "URL")]
    pub base_url: Option<String>,

    /// Install prefix; `vivi-<version>` and the `vivi` symlink live under
    /// `<PREFIX>/bin` (install.sh default: `~/.local`)
    #[arg(long, value_name = "DIR")]
    pub prefix: Option<PathBuf>,

    /// Move a plain-file `vivi` aside (`vivi.unmanaged`) instead of refusing
    #[arg(long, alias = "force")]
    pub adopt: bool,
}

/// List versioned installs next to the running binary.
#[derive(Debug, Args)]
pub struct VersionsCommand {
    /// Install prefix (install.sh default: `~/.local`)
    #[arg(long, value_name = "DIR")]
    pub prefix: Option<PathBuf>,
}

/// Remove versioned installs.
#[derive(Debug, Args)]
pub struct UninstallCommand {
    /// Remove one installed version
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,

    /// Remove every installed version and the `vivi` symlink
    #[arg(long, conflicts_with = "version")]
    pub all: bool,

    /// Install prefix (install.sh default: `~/.local`)
    #[arg(long, value_name = "DIR")]
    pub prefix: Option<PathBuf>,
}
