//! `vivi proxy check`: tiny live GET to `/<p>/health`, plus a streaming
//! probe for the `chatgpt-codex` adapter.

use std::time::Duration;

use serde_json::json;

use super::config::{self, BackendKind, ProviderConfig, ProxyConfig};
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
        if let Some(provider) = config.providers.get(name)
            && needs_stream_probe(provider)
            && let Err(reason) = stream_probe(&client, config, name, provider).await
        {
            failed = true;
            let url = stream_url(config, name);
            println!("{name} stream-check-failed {url}: {reason}");
        }
    }
    if failed {
        Err(VivariumError::Other("proxy check failed".into()))
    } else {
        Ok(())
    }
}

/// The streaming probe is native-codex only; other kinds stay health-only.
fn needs_stream_probe(provider: &ProviderConfig) -> bool {
    provider.kind == BackendKind::Native && provider.adapter.as_deref() == Some("chatgpt-codex")
}

fn stream_url(config: &ProxyConfig, name: &str) -> String {
    format!("http://{}/{name}/v1/responses", config.listen)
}

async fn stream_probe(
    client: &reqwest::Client,
    config: &ProxyConfig,
    name: &str,
    provider: &ProviderConfig,
) -> Result<(), String> {
    let url = stream_url(config, name);
    let model = provider
        .models
        .first()
        .map_or("gpt-6-luna", String::as_str);
    let body = json!({
        "model": model,
        "input": [{
            "role": "user",
            "content": [{"type": "input_text", "text": "Reply with exactly: ok"}],
        }],
        "stream": true,
        "temperature": 0.3,
    });
    let mut request = client
        .post(&url)
        .header("content-type", "application/json")
        .body(body.to_string());
    if let Some(cmd) = config.auth_token_cmd.as_deref() {
        let token = super::cmd::run_secret_cmd("auth_token_cmd", cmd).map_err(|e| e.to_string())?;
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("could not reach {url}: {error}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("http {status}"));
    }
    let text = response
        .text()
        .await
        .map_err(|error| format!("could not read stream: {error}"))?;
    validate_stream(&text)
}

fn validate_stream(text: &str) -> Result<(), String> {
    let mut buffer = text.as_bytes().to_vec();
    let events = super::sse::take_events(&mut buffer);
    let mut seen_data = false;
    for event in &events {
        match event.data() {
            Some(data) => {
                seen_data = true;
                if data != "[DONE]" && serde_json::from_str::<serde_json::Value>(&data).is_err() {
                    return Err(format!("event data is neither [DONE] nor JSON: {data:.80}"));
                }
            }
            None if event.name().is_some() => {
                return Err(format!("named frame without data: {:?}", event.raw));
            }
            None => {}
        }
    }
    if !seen_data {
        return Err("stream carried no data events".into());
    }
    let leftover = String::from_utf8_lossy(&buffer);
    if !leftover.trim().is_empty() {
        return Err(format!("incomplete trailing frame: {leftover:.80}"));
    }
    Ok(())
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
    let url = format!("http://{}/{name}/health", config.listen);
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
    let ok_http = response.status().is_success();
    let label = match response.text().await {
        Ok(text) => super::health::intern_status(&text, ok_http),
        Err(_) if ok_http => "ok",
        Err(_) => "down",
    };
    Ok((status, format!("{name} {status} {label} {url}")))
}
