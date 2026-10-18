//! Local loopback front door that turns subscription logins into one API.

pub mod access;
pub mod check;
pub mod child;
pub mod cmd;
pub mod codex;
pub mod codex_auth;
pub mod codex_chat;
pub mod codex_sse;
pub mod config;
pub mod credentials;
pub mod forward;
pub mod guard;
pub mod health;
pub mod http_error;
pub mod install;
pub mod jwt;
pub mod kimi;
pub mod native;
pub mod paths;
pub mod providers;
pub mod route;
pub mod runtime;
pub mod server;
pub mod sse;
pub mod status;
pub mod tap;
pub mod usage;

use std::path::Path;

use crate::VivariumError;
use crate::cli::{ProxyRunCommand, ProxySubcommand};

/// Dispatch a synchronous `vivi proxy` subcommand.
///
/// # Errors
///
/// Returns a configuration or I/O error from the chosen subcommand.
pub fn handle(command: &ProxySubcommand) -> Result<(), VivariumError> {
    match command {
        ProxySubcommand::Run(_) => Err(VivariumError::Other(
            "proxy run requires the async command entry point".into(),
        )),
        ProxySubcommand::Install(cmd) => install::install(cmd.config.as_deref()),
        ProxySubcommand::Uninstall(cmd) => install::uninstall(cmd.config.as_deref()),
        ProxySubcommand::Status(cmd) => status::print(cmd.config.as_deref()),
        ProxySubcommand::Log(cmd) => access::print_log(cmd),
        ProxySubcommand::Providers(cmd) => providers::print(cmd.config.as_deref()),
        ProxySubcommand::Check(cmd) => check::print(cmd),
    }
}

/// Run the foreground listener (launchd, systemd, or a terminal).
///
/// # Errors
///
/// Returns a configuration, bind, or supervisor error.
pub async fn run(cmd: &ProxyRunCommand) -> Result<(), VivariumError> {
    runtime::run(cmd.config.as_deref()).await
}

/// Resolve `--config`, then `VIVI_PROXY_CONFIG`, then the platform default.
///
/// # Errors
///
/// Returns an error when the platform config directory cannot be found.
pub fn resolve_config_path(explicit: Option<&Path>) -> Result<std::path::PathBuf, VivariumError> {
    paths::resolve_config_path(explicit)
}

#[cfg(test)]
#[path = "proxy/config_test.rs"]
mod config_test;

#[cfg(test)]
#[path = "proxy/route_test.rs"]
mod route_test;

#[cfg(test)]
#[path = "proxy/guard_test.rs"]
mod guard_test;

#[cfg(test)]
#[path = "proxy/jwt_test.rs"]
mod jwt_test;

#[cfg(test)]
#[path = "proxy/sse_test.rs"]
mod sse_test;

#[cfg(test)]
#[path = "proxy/codex_sse_test.rs"]
mod codex_sse_test;

#[cfg(test)]
#[path = "proxy/codex_chat_test.rs"]
mod codex_chat_test;

#[cfg(test)]
#[path = "proxy/credentials_test.rs"]
mod credentials_test;

#[cfg(test)]
#[path = "proxy/kimi_test.rs"]
mod kimi_test;

#[cfg(test)]
#[path = "proxy/usage_test.rs"]
mod usage_test;
