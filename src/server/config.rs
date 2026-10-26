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
        crate::hardening::scrub_env(&SECRET_NAMES);
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
            crate::safety::parse_send_rate(get("SEND_RATE_PER_MINUTE").as_deref())?;
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
mod tests {
    use super::*;

    fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vivi-server-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_listen_on_loopback_and_hold_no_secrets() {
        let config =
            ServerConfig::from_lookup(&lookup(&[("DATA_DIR", "/srv/vivi")]), None).unwrap();
        assert_eq!(config.bind, IpAddr::from([127, 0, 0, 1]));
        assert_eq!(config.port, 8080);
        assert_eq!(config.data_dir, PathBuf::from("/srv/vivi"));
        assert!(config.store_encryption_key.is_none());
        assert!(config.moderation_token.is_none());
        assert!(config.host_operator_token.is_none());
    }

    #[test]
    fn bind_names_the_interface_to_serve_on() {
        let env = [("DATA_DIR", "/srv/vivi"), ("BIND", "100.96.126.80")];
        let config = ServerConfig::from_lookup(&lookup(&env), None).unwrap();
        assert_eq!(config.bind, IpAddr::from([100, 96, 126, 80]));
        let bad = [("DATA_DIR", "/srv/vivi"), ("BIND", "tailnet")];
        let err = ServerConfig::from_lookup(&lookup(&bad), None).unwrap_err();
        assert_eq!(err, "invalid BIND: tailnet");
    }

    #[test]
    fn data_dir_is_required() {
        for env in [&[][..], &[("DATA_DIR", "  ")][..]] {
            let err = ServerConfig::from_lookup(&lookup(env), None).unwrap_err();
            assert!(err.starts_with("DATA_DIR is required"), "{err}");
        }
    }

    #[test]
    fn data_dir_must_be_absolute() {
        let err = ServerConfig::from_lookup(&lookup(&[("DATA_DIR", "./data")]), None).unwrap_err();
        assert_eq!(err, "DATA_DIR must be an absolute path: ./data");
    }

    #[test]
    fn a_credential_file_wins_over_the_environment_and_is_trimmed() {
        let dir = scratch("wins");
        std::fs::write(dir.join("MODERATION_TOKEN"), "from-file\n").unwrap();
        let env = [
            ("DATA_DIR", "/srv/vivi"),
            ("MODERATION_TOKEN", "from-env"),
            ("HOST_OPERATOR_TOKEN", "operator-env"),
        ];
        let config = ServerConfig::from_lookup(&lookup(&env), Some(&dir)).unwrap();
        assert_eq!(config.moderation_token.as_deref(), Some("from-file"));
        assert_eq!(config.host_operator_token.as_deref(), Some("operator-env"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_credential_is_an_error_not_a_fallback() {
        let dir = scratch("unreadable");
        std::fs::create_dir(dir.join("STORE_ENCRYPTION_KEY")).unwrap();
        let env = [
            ("DATA_DIR", "/srv/vivi"),
            ("STORE_ENCRYPTION_KEY", "from-env"),
        ];
        let err = ServerConfig::from_lookup(&lookup(&env), Some(&dir)).unwrap_err();
        assert!(err.starts_with("cannot read credential"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_blank_operator_token_counts_as_unset() {
        let dir = scratch("blank");
        std::fs::write(dir.join("HOST_OPERATOR_TOKEN"), "\n").unwrap();
        let env = [("DATA_DIR", "/srv/vivi")];
        let config = ServerConfig::from_lookup(&lookup(&env), Some(&dir)).unwrap();
        assert!(config.host_operator_token.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hosted_identity_keeps_its_old_rules() {
        let env = [
            ("DATA_DIR", "/srv/vivi"),
            ("HOSTED_DEFAULT_IDENTITY", " ops.1 "),
        ];
        let config = ServerConfig::from_lookup(&lookup(&env), None).unwrap();
        assert_eq!(config.hosted_default_identity.as_deref(), Some("ops.1"));
        let bad = [
            ("DATA_DIR", "/srv/vivi"),
            ("HOSTED_DEFAULT_IDENTITY", "a b"),
        ];
        assert!(ServerConfig::from_lookup(&lookup(&bad), None).is_err());
    }
}
