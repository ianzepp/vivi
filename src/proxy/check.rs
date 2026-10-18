//! `vivi proxy check`: tiny live GET to `/<p>/v1/models`.

use std::time::Duration;

use super::config::{self, ProxyConfig};
use crate::VivariumError;
use crate::cli::ProxyCheckCommand;

/// Probe one provider, or every provider.
///
/// # Errors
///
/// Returns a configuration or HTTP error.
pub fn print(cmd: &ProxyCheckCommand) -> Result<(), VivariumError> {
    let config = config::load(cmd.config.as_deref())?;
    let names = selected(&config, cmd.provider.as_deref())?;
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| VivariumError::Other(format!("runtime: {error}")))?;
        runtime.block_on(run_checks(&config, &names))
    })
    .join()
    .map_err(|_| VivariumError::Other("proxy check thread panicked".into()))?
}

async fn run_checks(config: &ProxyConfig, names: &[String]) -> Result<(), VivariumError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .no_proxy()
        .build()
        .map_err(|error| VivariumError::Other(format!("http client: {error}")))?;
    let mut failed = false;
    for name in names {
        match probe(&client, config, name).await {
            Ok((status, line)) => {
                println!("{line}");
                if !(200..300).contains(&status) {
                    failed = true;
                }
            }
            Err(error) => {
                failed = true;
                println!("{error}");
            }
        }
    }
    if failed {
        Err(VivariumError::Other("proxy check failed".into()))
    } else {
        Ok(())
    }
}

fn selected(config: &ProxyConfig, only: Option<&str>) -> Result<Vec<String>, VivariumError> {
    if let Some(name) = only {
        if config.providers.contains_key(name) {
            Ok(vec![name.to_string()])
        } else {
            Err(VivariumError::Config(format!("unknown provider {name}")))
        }
    } else {
        Ok(config.providers.keys().cloned().collect())
    }
}

async fn probe(
    client: &reqwest::Client,
    config: &ProxyConfig,
    name: &str,
) -> Result<(u16, String), VivariumError> {
    let url = format!("http://{}/{name}/v1/models", config.listen);
    let mut request = client.get(&url);
    if let Some(cmd) = config.auth_token_cmd.as_deref() {
        let token = super::cmd::run_secret_cmd("auth_token_cmd", cmd)?;
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|error| VivariumError::Other(format!("could not reach {url}: {error}")))?;
    let status = response.status().as_u16();
    Ok((status, format!("{name} {status} {url}")))
}
