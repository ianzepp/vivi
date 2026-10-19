//! The Fly Machines HTTP API, for what `flyctl` cannot do atomically or
//! cheaply: leases, metadata writes, config-file delivery, start/stop and
//! exec inside a machine.
//!
//! The API token comes from `FLY_API_TOKEN`, else `fly auth token`. It is held
//! as a [`Secret`] and never appears in an error.

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::fly::{RawMachine, record_from_raw};
use super::fly_token::{TOKEN_MAX_AGE, TokenCache};
use super::github::Secret;
use super::provider::{ExecOutput, Hold, MachineRecord};
use crate::b64;
use crate::error::VivariumError;

const BASE: &str = "https://api.machines.dev/v1/apps";
const USER_AGENT: &str = "vivi-pool";
const RATE_LIMIT_RETRIES: u64 = 4;

/// A machines-API client for one app.
#[derive(Debug, Clone)]
pub struct FlyApi {
    app: String,
    base: String,
    tokens: Arc<TokenCache>,
}

#[derive(Debug)]
struct Reply {
    status: u16,
    body: Value,
}

impl FlyApi {
    #[must_use]
    pub fn new(app: &str) -> Self {
        Self::with_parts(app, BASE, fetch_token)
    }

    /// A client against another base URL with another token source (tests).
    #[must_use]
    pub(super) fn with_parts(
        app: &str,
        base: &str,
        fetch: fn() -> Result<Secret, VivariumError>,
    ) -> Self {
        Self {
            app: app.to_string(),
            base: base.to_string(),
            tokens: Arc::new(TokenCache::new(TOKEN_MAX_AGE, fetch)),
        }
    }

    fn authorization(&self) -> Result<Secret, VivariumError> {
        let token = self.tokens.get()?;
        let raw = token.expose();
        Ok(Secret::new(if raw.starts_with("FlyV1 ") {
            raw.to_string()
        } else {
            format!("Bearer {raw}")
        }))
    }

    /// One request, with a fresh token after the API refuses the cached one
    /// and a pause after a rate limit.
    fn call(
        &self,
        method: &'static str,
        path: &str,
        nonce: Option<&str>,
        body: Option<&Value>,
        read_timeout: Duration,
    ) -> Result<Reply, VivariumError> {
        let url = format!("{}/{}{path}", self.base, self.app);
        let mut auth = self.authorization()?;
        let mut renewed = false;
        for attempt in 0..=RATE_LIMIT_RETRIES {
            let reply = send_once(method, &url, &auth, nonce, body, read_timeout)?;
            if matches!(reply.status, 401 | 403) && !renewed {
                renewed = true;
                self.tokens.invalidate();
                auth = self.authorization()?;
                continue;
            }
            if reply.status != 429 || attempt == RATE_LIMIT_RETRIES {
                return Ok(reply);
            }
            std::thread::sleep(Duration::from_secs(2 * (attempt + 1)));
        }
        Err(VivariumError::Message(
            "Fly rate limit did not clear".into(),
        ))
    }

    fn require(
        &self,
        method: &'static str,
        path: &str,
        nonce: Option<&str>,
        body: Option<&Value>,
        ok: &[u16],
    ) -> Result<Reply, VivariumError> {
        let reply = self.call(method, path, nonce, body, Duration::from_secs(40))?;
        if ok.contains(&reply.status) {
            return Ok(reply);
        }
        Err(VivariumError::Message(format!(
            "Fly {method} {path} failed (HTTP {}): {}",
            reply.status,
            reply_message(&reply.body)
        )))
    }

    /// One machine, with its config. A machine the API no longer knows (HTTP
    /// 404) reads as [`MachineRecord::gone`]: destroyed.
    ///
    /// # Errors
    /// Returns an error if the call fails for any other reason.
    pub fn machine(&self, id: &str) -> Result<MachineRecord, VivariumError> {
        let reply = self.require("GET", &format!("/machines/{id}"), None, None, &[200, 404])?;
        if reply.status == 404 {
            return Ok(MachineRecord::gone(id));
        }
        let raw: RawMachine = serde_json::from_value(reply.body)
            .map_err(|e| VivariumError::Parse(format!("unexpected machine record: {e}")))?;
        Ok(record_from_raw(raw))
    }

    /// Take a lease; `None` when someone else holds it.
    ///
    /// # Errors
    /// Returns an error for any failure other than "already held".
    pub fn lease(&self, id: &str, ttl_secs: u32) -> Result<Option<Hold>, VivariumError> {
        let path = format!("/machines/{id}/lease");
        let reply = self.call(
            "POST",
            &path,
            None,
            Some(&json!({"ttl": ttl_secs})),
            Duration::from_secs(40),
        )?;
        match reply.status {
            200 | 201 => Ok(reply.body["data"]["nonce"].as_str().map(|nonce| Hold {
                nonce: nonce.to_string(),
            })),
            409 | 412 => Ok(None),
            status => Err(VivariumError::Message(format!(
                "Fly POST {path} failed (HTTP {status}): {}",
                reply_message(&reply.body)
            ))),
        }
    }

    /// Release a lease.
    ///
    /// # Errors
    /// Returns an error if the release is refused.
    pub fn release(&self, id: &str, lease: &Hold) -> Result<(), VivariumError> {
        let path = format!("/machines/{id}/lease");
        self.require("DELETE", &path, Some(&lease.nonce), None, &[200, 204])
            .map(|_| ())
    }

    /// Set a metadata key.
    ///
    /// # Errors
    /// Returns an error if the write is refused.
    pub fn set_metadata(&self, id: &str, key: &str, value: &str) -> Result<(), VivariumError> {
        let path = format!("/machines/{id}/metadata/{key}");
        self.require(
            "POST",
            &path,
            None,
            Some(&json!({"value": value})),
            &[200, 204],
        )
        .map(|_| ())
    }

    /// Delete a metadata key; a key that is not there is fine.
    ///
    /// # Errors
    /// Returns an error if the delete is refused.
    pub fn clear_metadata(&self, id: &str, key: &str) -> Result<(), VivariumError> {
        let path = format!("/machines/{id}/metadata/{key}");
        self.require("DELETE", &path, None, None, &[200, 204, 404])
            .map(|_| ())
    }

    /// Write a config file into a stopped machine without starting it.
    ///
    /// # Errors
    /// Returns an error if the config cannot be read or the update is refused.
    pub fn put_file(
        &self,
        id: &str,
        lease: &Hold,
        path: &str,
        content: &[u8],
    ) -> Result<(), VivariumError> {
        let mut config = self.config(id)?;
        upsert_file(&mut config, path, content);
        self.update(id, Some(&lease.nonce), &config)
    }

    /// Drop environment names and set one metadata key in a single config
    /// update of a stopped machine; the machine stays stopped.
    ///
    /// # Errors
    /// Returns an error if the config cannot be read or the update is refused.
    pub fn seal(
        &self,
        id: &str,
        drop_env: &[&str],
        key: &str,
        value: &str,
    ) -> Result<(), VivariumError> {
        let mut config = self.config(id)?;
        drop_env_names(&mut config, drop_env);
        set_config_metadata(&mut config, key, value);
        self.update(id, None, &config)
    }

    fn config(&self, id: &str) -> Result<Value, VivariumError> {
        let reply = self.require("GET", &format!("/machines/{id}"), None, None, &[200])?;
        Ok(reply.body["config"].clone())
    }

    fn update(&self, id: &str, nonce: Option<&str>, config: &Value) -> Result<(), VivariumError> {
        let body = json!({"config": config, "skip_launch": true});
        self.require(
            "POST",
            &format!("/machines/{id}"),
            nonce,
            Some(&body),
            &[200],
        )
        .map(|_| ())
    }

    /// Start a machine.
    ///
    /// # Errors
    /// Returns an error if the start is refused.
    pub fn start(&self, id: &str) -> Result<(), VivariumError> {
        self.require("POST", &format!("/machines/{id}/start"), None, None, &[200])
            .map(|_| ())
    }

    /// Start a claimed machine using the lease that protects its delivery.
    ///
    /// # Errors
    /// Returns an error if the start is refused.
    pub fn start_claimed(&self, id: &str, hold: &Hold) -> Result<(), VivariumError> {
        self.require(
            "POST",
            &format!("/machines/{id}/start"),
            Some(&hold.nonce),
            None,
            &[200],
        )
        .map(|_| ())
    }

    /// Stop a machine.
    ///
    /// # Errors
    /// Returns an error if the stop is refused.
    pub fn stop(&self, id: &str) -> Result<(), VivariumError> {
        let body = json!({"signal": "SIGTERM", "timeout": "20s"});
        self.require(
            "POST",
            &format!("/machines/{id}/stop"),
            None,
            Some(&body),
            &[200],
        )
        .map(|_| ())
    }

    /// Run a script inside the machine.
    ///
    /// # Errors
    /// Returns an error if the machine is not running or the call fails.
    pub fn exec(
        &self,
        id: &str,
        script: &str,
        timeout_secs: u32,
    ) -> Result<ExecOutput, VivariumError> {
        let body = json!({"command": ["bash", "-c", script], "timeout": timeout_secs});
        let path = format!("/machines/{id}/exec");
        let wait = Duration::from_secs(u64::from(timeout_secs) + 15);
        let reply = self.call("POST", &path, None, Some(&body), wait)?;
        if reply.status != 200 {
            return Err(VivariumError::Message(format!(
                "Fly POST {path} failed (HTTP {}): {}",
                reply.status,
                reply_message(&reply.body)
            )));
        }
        Ok(ExecOutput {
            stdout: reply.body["stdout"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            stderr: reply.body["stderr"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            exit_code: reply.body["exit_code"].as_i64().unwrap_or(-1),
        })
    }
}

/// Add or replace one file in a machine config's `files` list.
pub fn upsert_file(config: &mut Value, path: &str, content: &[u8]) {
    let entry = json!({"guest_path": path, "raw_value": b64::encode(content)});
    let mut files: Vec<Value> = config["files"].as_array().cloned().unwrap_or_default();
    files.retain(|f| f["guest_path"].as_str() != Some(path));
    files.push(entry);
    config["files"] = Value::Array(files);
}

/// Set one metadata key in a machine config.
pub fn set_config_metadata(config: &mut Value, key: &str, value: &str) {
    if !config["metadata"].is_object() {
        config["metadata"] = json!({});
    }
    config["metadata"][key] = Value::String(value.to_string());
}

/// Remove environment names from a machine config.
pub fn drop_env_names(config: &mut Value, names: &[&str]) {
    if let Some(env) = config.get_mut("env").and_then(Value::as_object_mut) {
        for name in names {
            env.remove(*name);
        }
    }
}

pub(super) fn fetch_token() -> Result<Secret, VivariumError> {
    for var in ["FLY_API_TOKEN", "FLY_ACCESS_TOKEN"] {
        if let Ok(value) = std::env::var(var)
            && !value.trim().is_empty()
        {
            return Ok(Secret::new(value.trim().to_string()));
        }
    }
    let output = Command::new("fly")
        .args(["auth", "token"])
        .output()
        .map_err(|e| {
            VivariumError::Message(format!(
                "failed to run `fly auth token` (is flyctl on PATH?): {e}"
            ))
        })?;
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || token.is_empty() {
        return Err(VivariumError::Message(
            "no Fly API token: set FLY_API_TOKEN or run `fly auth login`".into(),
        ));
    }
    Ok(Secret::new(token))
}

fn reply_message(body: &Value) -> String {
    ["message", "error"]
        .iter()
        .find_map(|k| body[*k].as_str())
        .unwrap_or("no message")
        .to_string()
}

fn send_once(
    method: &'static str,
    url: &str,
    auth: &Secret,
    nonce: Option<&str>,
    body: Option<&Value>,
    read_timeout: Duration,
) -> Result<Reply, VivariumError> {
    let (u, a, n, b) = (
        url.to_string(),
        auth.clone(),
        nonce.map(str::to_string),
        body.cloned(),
    );
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| VivariumError::Other(format!("failed to build runtime: {e}")))?;
        runtime.block_on(send(method, &u, &a, n.as_deref(), b, read_timeout))
    })
    .join()
    .map_err(|_| VivariumError::Message("Fly request thread panicked".into()))?
}

async fn send(
    method: &'static str,
    url: &str,
    auth: &Secret,
    nonce: Option<&str>,
    body: Option<Value>,
    timeout: Duration,
) -> Result<Reply, VivariumError> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| VivariumError::Other(format!("failed to build client: {e}")))?;
    let mut call = client
        .request(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET),
            url,
        )
        .header("Authorization", auth.expose());
    if let Some(nonce) = nonce {
        call = call.header("fly-machine-lease-nonce", nonce);
    }
    if let Some(body) = body {
        call = call.json(&body);
    }
    let response = call
        .send()
        .await
        .map_err(|e| VivariumError::Message(format!("Fly unreachable: {}", e.without_url())))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let body = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok(Reply { status, body })
}

#[cfg(test)]
#[path = "fly_api_test.rs"]
mod tests;
