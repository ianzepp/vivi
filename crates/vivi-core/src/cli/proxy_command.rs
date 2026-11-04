//! `vivi proxy`: local loopback front door for model providers.

use std::path::PathBuf;

use clap::{Args, Subcommand};

/// User-level model proxy.
#[derive(Debug, Subcommand)]
pub enum ProxySubcommand {
    /// Run the foreground listener and supervisor
    Run(ProxyRunCommand),
    /// Install and start the current binary as a user OS service
    Install(ProxyConfigArg),
    /// Stop and remove the user OS service
    Uninstall(ProxyConfigArg),
    /// Show OS service-manager status and listener state
    Status(ProxyConfigArg),
    /// Print the rotated JSONL access log (or a child provider log)
    Log(ProxyLogCommand),
    /// List configured providers, Pi base URLs, health, and credential expiry
    Providers(ProxyConfigArg),
    /// Send a tiny live request to one provider, or every provider
    Check(ProxyCheckCommand),
}

/// Shared `--config` override for proxy commands.
#[derive(Debug, Args)]
pub struct ProxyConfigArg {
    /// Path to `proxy.toml` (default: platform config dir + `vivi/proxy.toml`)
    #[arg(long)]
    pub config: Option<PathBuf>,
}

/// Foreground `vivi proxy run`.
#[derive(Debug, Args)]
pub struct ProxyRunCommand {
    /// Path to `proxy.toml` (default: platform config dir + `vivi/proxy.toml`)
    #[arg(long)]
    pub config: Option<PathBuf>,
}

/// `vivi proxy log`.
#[derive(Debug, Args)]
pub struct ProxyLogCommand {
    /// Number of lines to print, counted after filtering
    #[arg(short = 'n', long = "lines", default_value_t = 50)]
    pub lines: usize,
    /// Keep printing lines as they are appended
    #[arg(short = 'f', long)]
    pub follow: bool,
    /// Child provider stdout/stderr instead of the access log
    #[arg(long)]
    pub provider: Option<String>,
    /// Path to `proxy.toml`
    #[arg(long)]
    pub config: Option<PathBuf>,
}

/// `vivi proxy check [provider]`.
#[derive(Debug, Args)]
pub struct ProxyCheckCommand {
    /// Provider name; omit to check every configured provider
    pub provider: Option<String>,
    /// Path to `proxy.toml`
    #[arg(long)]
    pub config: Option<PathBuf>,
}
