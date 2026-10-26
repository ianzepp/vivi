//! The server's configuration: environment variables, plus systemd credential
//! files for the three secrets.
//!
//! A secret is read from `$CREDENTIALS_DIRECTORY/<NAME>` when that file
//! exists (systemd `LoadCredential=NAME:/path`), and from the environment
//! variable `NAME` otherwise. The credential file wins. A secret taken from
//! the environment is removed from it after reading, so a child process the
//! server starts does not inherit it.

use std::io::ErrorKind;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

/// The variables that hold secrets: each may come from a credential file.
pub const SECRET_NAMES: [&str; 3] = [
    "STORE_ENCRYPTION_KEY",
    "MODERATION_TOKEN",
    "HOST_OPERATOR_TOKEN",
];

/// Configuration read at boot. The process refuses to boot on a bad value.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Listen address (`BIND`, default `127.0.0.1`). A host that serves
    /// other machines names the interface it serves them on.
    pub bind: IpAddr,
    pub port: u16,
    /// `DATA_DIR`: required, and absolute, so a service started from an
    /// unexpected working directory never writes somewhere else.
    pub data_dir: PathBuf,
    pub store_encryption_key: Option<String>,
    pub moderation_token: Option<String>,
    /// `HOSTED_DEFAULT_IDENTITY`: the vivi identity an API key without its
    /// own identity acts as on a hosted project.
    pub hosted_default_identity: Option<String>,
    /// `HOST_OPERATOR_TOKEN`: a built-in host principal on `POST /v1/mcp`.
    pub host_operator_token: Option<String>,
    /// `SEND_RATE_PER_MINUTE`: writes per minute per identity (default 30;
    /// `0` is unlimited, held as `None`).
    pub send_rate_per_minute: Option<u32>,
}

impl ServerConfig {
    /// Read the process environment and `$CREDENTIALS_DIRECTORY`, then remove
    /// the secrets from the environment.
    ///
    /// Call it before any other thread exists: removing a variable is not
    /// safe beside a thread that reads the environment.
    ///
    /// # Errors
    /// Returns a message naming the first bad or unreadable setting.
    pub fn from_env() -> Result<Self, String> {
        let credentials = std::env::var_os("CREDENTIALS_DIRECTORY").map(PathBuf::from);
        let config = Self::from_lookup(&|name| std::env::var(name).ok(), credentials.as_deref())?;
        crate::server::hardening::scrub_env(&SECRET_NAMES);
        Ok(config)
    }

    /// Build the configuration from `get` (the environment) and the
    /// credential directory, if any.
    ///
    /// # Errors
    /// Returns a message naming the first bad or unreadable setting.
    pub fn from_lookup(
        get: &dyn Fn(&str) -> Option<String>,
        credentials: Option<&Path>,
    ) -> Result<Self, String> {
        let port = match get("PORT") {
            Some(v) => v.parse::<u16>().map_err(|_| format!("invalid PORT: {v}"))?,
            None => 8080,
        };
        let bind = match get("BIND") {
            Some(v) => v
                .parse::<IpAddr>()
                .map_err(|_| format!("invalid BIND: {v}"))?,
            None => IpAddr::from([127, 0, 0, 1]),
        };
        let send_rate_per_minute =
            crate::server::safety::parse_send_rate(get("SEND_RATE_PER_MINUTE").as_deref())?;
        Ok(Self {
            bind,
            port,
            data_dir: data_dir(get("DATA_DIR"))?,
            send_rate_per_minute,
            store_encryption_key: secret("STORE_ENCRYPTION_KEY", get, credentials)?,
            moderation_token: secret("MODERATION_TOKEN", get, credentials)?,
            host_operator_token: secret("HOST_OPERATOR_TOKEN", get, credentials)?
                .filter(|t| !t.trim().is_empty()),
            hosted_default_identity: hosted_identity(get("HOSTED_DEFAULT_IDENTITY"))?,
        })
    }
}

fn data_dir(value: Option<String>) -> Result<PathBuf, String> {
    let Some(value) = value.filter(|v| !v.trim().is_empty()) else {
        return Err("DATA_DIR is required (an absolute path)".into());
    };
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(format!(
            "DATA_DIR must be an absolute path: {}",
            path.display()
        ));
    }
    Ok(path)
}

fn hosted_identity(value: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = value.filter(|v| !v.trim().is_empty()) else {
        return Ok(None);
    };
    let value = value.trim().to_owned();
    let plain = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if value.len() > 64 || !plain {
        return Err(format!("invalid HOSTED_DEFAULT_IDENTITY: {value}"));
    }
    Ok(Some(value))
}

/// One secret: the credential file `name` when it exists, else the
/// environment variable `name`.
fn secret(
    name: &str,
    get: &dyn Fn(&str) -> Option<String>,
    credentials: Option<&Path>,
) -> Result<Option<String>, String> {
    if let Some(dir) = credentials {
        let path = dir.join(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => return Ok(Some(text.trim().to_owned())),
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(format!("cannot read credential {}: {err}", path.display())),
        }
    }
    Ok(get(name))
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
