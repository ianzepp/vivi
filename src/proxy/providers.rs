//! `vivi proxy providers`: Pi base URLs, health, credential expiry.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

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
    format!(
        "{name}  {kind}  {base}  health={}  credentials={credentials}",
        health_label(config.listen, name)
    )
}

fn health_label(listen: SocketAddr, name: &str) -> &'static str {
    match probe_listen_health(listen, name) {
        Ok(true) => "ok",
        Ok(false) | Err(_) => "down",
    }
}

fn probe_listen_health(listen: SocketAddr, name: &str) -> Result<bool, VivariumError> {
    let url = format!("http://{listen}/{name}/health");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .no_proxy()
        .build()
        .map_err(|error| VivariumError::Other(format!("http client: {error}")))?;
    let response = client
        .get(&url)
        .send()
        .map_err(|error| VivariumError::Message(format!("could not reach {url}: {error}")))?;
    Ok(response.status().is_success())
}
