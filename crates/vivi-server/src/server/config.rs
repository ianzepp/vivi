//! The server's configuration: one file, `$VIVI_HOME/server/config.toml`
//! (`VIVI_HOME` defaults to `~/.vivi`). No other environment variable is
//! read. Secrets are files in `$VIVI_HOME/server/creds/<NAME>`.
//!
//! ```toml
//! bind = "127.0.0.1"          # default
//! port = 8080                 # default
//! send_rate_per_minute = 0    # default 30; 0 is unlimited
//! hosted_default_identity = "operator"
//! log = "vivi::server=info,vivi::pump=info,tower_http=info"
//! data_dir = "data"           # relative paths are under $VIVI_HOME/server
//! projects_root = "projects"
//! lanes_root = "lanes"
//! git_url_prefix = "file:///abs/lanes/"   # default: file://<lanes_root>/
//! default_pool = "studio"
//! ```

use std::io::ErrorKind;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::server::mcp::host::HostConfig;

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
    /// The host tools' layout (projects, lanes, git URL prefix, pool).
    pub host: HostConfig,
    /// Log filter (`log`).
    pub log: String,
    /// `$VIVI_HOME/server`.
    pub home: PathBuf,
}

/// The file's shape. Every key is optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    bind: Option<IpAddr>,
    port: Option<u16>,
    send_rate_per_minute: Option<u32>,
    hosted_default_identity: Option<String>,
    log: Option<String>,
    data_dir: Option<PathBuf>,
    projects_root: Option<PathBuf>,
    lanes_root: Option<PathBuf>,
    git_url_prefix: Option<String>,
    default_pool: Option<String>,
}

pub use vivi_util::home::vivi_home;

impl ServerConfig {
    /// Read `$VIVI_HOME/server/config.toml` and the credential files beside it.
    /// A missing file means every default.
    ///
    /// # Errors
    /// Returns a message naming the first bad or unreadable setting.
    pub fn load() -> Result<Self, String> {
        Self::load_from(&vivi_util::home::server_home()?)
    }

    /// Read the configuration of the server home `home`.
    ///
    /// # Errors
    /// Returns a message naming the first bad or unreadable setting.
    pub fn load_from(home: &Path) -> Result<Self, String> {
        let path = home.join("config.toml");
        let file: FileConfig = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?,
            Err(err) if err.kind() == ErrorKind::NotFound => FileConfig::default(),
            Err(err) => return Err(format!("cannot read {}: {err}", path.display())),
        };
        let under = |value: Option<PathBuf>, default: &str| {
            let value = value.unwrap_or_else(|| PathBuf::from(default));
            if value.is_absolute() {
                value
            } else {
                home.join(value)
            }
        };
        let lanes_root = under(file.lanes_root, "lanes");
        let git_url_prefix = file
            .git_url_prefix
            .unwrap_or_else(|| format!("file://{}/", lanes_root.display()));
        let creds = home.join("creds");
        Ok(Self {
            bind: file.bind.unwrap_or(IpAddr::from([127, 0, 0, 1])),
            port: file.port.unwrap_or(8080),
            data_dir: under(file.data_dir, "data"),
            send_rate_per_minute: crate::server::safety::parse_send_rate(
                file.send_rate_per_minute.map(|n| n.to_string()).as_deref(),
            )?,
            store_encryption_key: secret(&creds, "STORE_ENCRYPTION_KEY")?,
            moderation_token: secret(&creds, "MODERATION_TOKEN")?,
            host_operator_token: secret(&creds, "HOST_OPERATOR_TOKEN")?
                .filter(|t| !t.trim().is_empty()),
            hosted_default_identity: hosted_identity(file.hosted_default_identity)?,
            host: HostConfig {
                projects_root: Some(under(file.projects_root, "projects")),
                lanes_root: Some(lanes_root),
                git_url_prefix: Some(git_url_prefix),
                default_pool: file.default_pool.map(|v| v.trim().to_owned()),
            },
            log: file
                .log
                .unwrap_or_else(|| "vivi::server=info,vivi::pump=info,tower_http=info".into()),
            home: home.to_path_buf(),
        })
    }
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
        return Err(format!("invalid hosted_default_identity: {value}"));
    }
    Ok(Some(value))
}

/// One secret: the file `creds/<name>`, or none.
fn secret(creds: &Path, name: &str) -> Result<Option<String>, String> {
    let path = creds.join(name);
    #[cfg(unix)]
    {
        check_private(creds, creds)?;
        if path.exists() {
            check_private(creds, &path)?;
        }
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text.trim().to_owned())),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("cannot read credential {}: {err}", path.display())),
    }
}

/// Refuse a credential path that group or others can read, like ssh does.
#[cfg(unix)]
fn check_private(creds: &Path, path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    if meta.permissions().mode() & 0o077 != 0 {
        let creds = creds.display();
        return Err(format!(
            "{} is readable by other users; run: chmod 700 {creds} && chmod 600 {creds}/*",
            path.display()
        ));
    }
    Ok(())
}

/// The machine keys listed in `host_keys` of the server home `home`,
/// normalized to base64. A missing file lists none.
#[must_use]
pub fn host_key_list(home: &Path) -> Vec<String> {
    use base64::Engine;
    let Ok(text) = std::fs::read_to_string(home.join("host_keys")) else {
        return Vec::new();
    };
    text.lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter_map(|line| {
            line.split_whitespace()
                .find_map(crate::server::machine_auth::decode_key)
        })
        .map(|key| base64::engine::general_purpose::STANDARD.encode(key))
        .collect()
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
