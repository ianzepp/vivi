//! `vivi proxy providers`: Pi base URLs, health, credential expiry.

use std::path::Path;

use super::config::{self, BackendKind, ProviderConfig, ProxyConfig};
use super::native;
use crate::VivariumError;

/// Print one line per provider.
///
/// # Errors
///
/// Returns a configuration error when `proxy.toml` cannot be read.
pub fn print(explicit: Option<&Path>) -> Result<(), VivariumError> {
    let config = config::load(explicit)?;
    if config.providers.is_empty() {
        println!("no providers");
        return Ok(());
    }
    for (name, provider) in &config.providers {
        println!("{}", format_row(&config, name, provider));
    }
    Ok(())
}

/// One provider row. Credential column never contains a token.
#[must_use]
pub fn format_row(config: &ProxyConfig, name: &str, provider: &ProviderConfig) -> String {
    let kind = match provider.kind {
        BackendKind::Native => "native",
        BackendKind::Child => "child",
        BackendKind::Upstream => "upstream",
    };
    let base = format!("http://{}/{name}/v1", config.listen);
    let credentials = match provider.kind {
        BackendKind::Native => native::credential_state(provider),
        _ => "-".into(),
    };
    format!("{name}  {kind}  {base}  credentials={credentials}")
}
