//! GitHub App installation tokens and base-commit resolution.
//!
//! The App's RS256 JWT is signed by shelling out to `openssl dgst -sha256
//! -sign`: Vivi already carries a base64 helper and `reqwest`, and `openssl`
//! is a stock tool, so no signing crate joins the dependency tree. The key is
//! read by `openssl` itself; Vivi never loads it. A token is wrapped in
//! [`Secret`] so it cannot reach a log or a `Debug` print by accident.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::config::PoolConfig;
use crate::chat::b64;
use crate::error::VivariumError;

const API: &str = "https://api.github.com";
const USER_AGENT: &str = "vivi-pool";

/// A credential that never prints.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The raw value, for handing to the one place that needs it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Base64url without padding, as JWT segments require.
#[must_use]
pub fn b64url(bytes: &[u8]) -> String {
    b64::encode(bytes)
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_string()
}

/// The `header.payload` part of the App JWT, valid from 30s before `now_secs`
/// for nine minutes (GitHub allows ten).
#[must_use]
pub fn jwt_signing_input(app_id: u64, now_secs: u64) -> String {
    let header = json!({"alg": "RS256", "typ": "JWT"});
    let claims = json!({
        "iat": now_secs.saturating_sub(30),
        "exp": now_secs + 540,
        "iss": app_id.to_string(),
    });
    format!(
        "{}.{}",
        b64url(header.to_string().as_bytes()),
        b64url(claims.to_string().as_bytes())
    )
}

/// RS256-sign `input` with the PEM key at `key`, using `openssl`.
///
/// # Errors
/// Returns an error if `openssl` is missing, the key is unreadable, or signing fails.
pub fn sign_rs256(key: &Path, input: &str) -> Result<Vec<u8>, VivariumError> {
    let mut child = Command::new("openssl")
        .args(["dgst", "-sha256", "-sign"])
        .arg(key)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| VivariumError::Message(format!("failed to run openssl: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes())?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(VivariumError::Message(format!(
            "openssl could not sign with {}: {}",
            key.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

fn app_jwt(app_id: u64, key: &Path) -> Result<Secret, VivariumError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| VivariumError::Other(format!("clock before epoch: {e}")))?
        .as_secs();
    let input = jwt_signing_input(app_id, now);
    let signature = sign_rs256(key, &input)?;
    Ok(Secret(format!("{input}.{}", b64url(&signature))))
}

/// Mint a one-hour installation token narrowed to the pool's repo with
/// `contents: write`.
///
/// # Errors
/// Returns an error if signing, the HTTP calls, or the response shape fail.
pub fn mint_token(config: &PoolConfig) -> Result<Secret, VivariumError> {
    let jwt = app_jwt(config.github.app_id, &config.private_key_path())?;
    let install = request(
        "GET",
        &format!("{API}/repos/{}/installation", config.repo),
        &jwt,
        None,
    )?;
    let id = install["id"].as_u64().ok_or_else(|| {
        VivariumError::Message(format!(
            "GitHub App is not installed on {} (no installation id)",
            config.repo
        ))
    })?;
    let name = config.repo.rsplit('/').next().unwrap_or_default();
    let body = json!({"repositories": [name], "permissions": {"contents": "write"}});
    let url = format!("{API}/app/installations/{id}/access_tokens");
    let reply = request("POST", &url, &jwt, Some(body))?;
    reply["token"]
        .as_str()
        .map(|t| Secret(t.to_string()))
        .ok_or_else(|| VivariumError::Message("GitHub returned no installation token".into()))
}

/// Resolve `requested` (a commit, branch, or tag; `None` for the default
/// branch tip) to a full commit hash.
///
/// # Errors
/// Returns an error if the ref is malformed, unknown, or GitHub is unreachable.
pub fn resolve_base(
    token: &Secret,
    repo: &str,
    requested: Option<&str>,
) -> Result<String, VivariumError> {
    let reference = if let Some(r) = requested {
        check_ref(r)?;
        r.to_string()
    } else {
        default_branch(token, repo)?
    };
    let commit = request(
        "GET",
        &format!("{API}/repos/{repo}/commits/{reference}"),
        token,
        None,
    )?;
    let sha = commit["sha"].as_str().unwrap_or_default();
    if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(sha.to_string())
    } else {
        Err(VivariumError::Message(format!(
            "could not resolve \"{reference}\" to a commit in {repo}"
        )))
    }
}

/// The repository's default branch, as GitHub names it.
///
/// # Errors
/// Returns an error if GitHub is unreachable or names no default branch.
pub fn default_branch(token: &Secret, repo: &str) -> Result<String, VivariumError> {
    let info = request("GET", &format!("{API}/repos/{repo}"), token, None)?;
    info["default_branch"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| VivariumError::Message(format!("no default branch for {repo}")))
}

fn check_ref(reference: &str) -> Result<(), VivariumError> {
    let ok = !reference.is_empty()
        && reference
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
    if ok {
        Ok(())
    } else {
        Err(VivariumError::Message(format!(
            "base \"{reference}\" is not a plain commit, branch, or tag name"
        )))
    }
}

/// One JSON request on a private runtime thread (the CLI entrypoint already
/// owns a runtime, so blocking inside it is not allowed).
fn request(
    method: &'static str,
    url: &str,
    bearer: &Secret,
    body: Option<Value>,
) -> Result<Value, VivariumError> {
    let url = url.to_string();
    let bearer = bearer.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| VivariumError::Other(format!("failed to build runtime: {e}")))?;
        runtime.block_on(send(method, &url, &bearer, body))
    })
    .join()
    .map_err(|_| VivariumError::Message("GitHub request thread panicked".into()))?
}

async fn send(
    method: &'static str,
    url: &str,
    bearer: &Secret,
    body: Option<Value>,
) -> Result<Value, VivariumError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| VivariumError::Other(format!("failed to build client: {e}")))?;
    let mut call = client
        .request(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET),
            url,
        )
        .bearer_auth(bearer.expose())
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(body) = body {
        call = call.json(&body);
    }
    let response = call
        .send()
        .await
        .map_err(|e| VivariumError::Message(format!("GitHub unreachable: {}", e.without_url())))?;
    let status = response.status();
    if !status.is_success() {
        return Err(VivariumError::Message(format!(
            "GitHub {method} {} failed (HTTP {})",
            url.trim_start_matches(API),
            status.as_u16()
        )));
    }
    response
        .json::<Value>()
        .await
        .map_err(|e| VivariumError::Message(format!("invalid GitHub response: {e}")))
}

#[cfg(test)]
#[path = "github_test.rs"]
mod tests;
