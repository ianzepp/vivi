//! This machine's ed25519 key, `$VIVI_HOME/keys/machine.ed25519` (a 32-byte
//! seed, mode 600, made on first use), and sign-in to a vivi server with it.
//!
//! Sign-in asks the server for a challenge, signs it, and receives a session
//! token for `/v1/mcp`. The token is cached under `$VIVI_HOME/keys/sessions/`
//! until a minute before it expires.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use rand_core::OsRng;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use vivi_error::VivariumError;

/// What the machine signs: this prefix, then the raw nonce bytes. Must match
/// the server's `machine_auth::MESSAGE_PREFIX`.
const MESSAGE_PREFIX: &[u8] = b"vivi machine sign-in v1\n";

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn err(message: impl Into<String>) -> VivariumError {
    VivariumError::Message(message.into())
}

fn keys_dir() -> Result<PathBuf, VivariumError> {
    Ok(vivi_util::home::vivi_home().map_err(err)?.join("keys"))
}

fn private_dir(path: &PathBuf) -> Result<(), VivariumError> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Refuse a key file or keys directory that group or others can reach.
#[cfg(unix)]
fn refuse_open_modes(dir: &PathBuf, path: &PathBuf) -> Result<(), VivariumError> {
    if fs::metadata(dir)?.permissions().mode() & 0o077 != 0 {
        return Err(err(format!(
            "{} is accessible by group or others; fix with: chmod 700 {}",
            dir.display(),
            dir.display()
        )));
    }
    if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
        return Err(err(format!(
            "{} is readable by group or others; fix with: chmod 600 {}",
            path.display(),
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn refuse_open_modes(_dir: &PathBuf, _path: &PathBuf) -> Result<(), VivariumError> {
    Ok(())
}

fn private_write(path: &PathBuf, bytes: &[u8]) -> Result<(), VivariumError> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// The machine key.
pub struct MachineKey {
    signing_key: SigningKey,
    path: PathBuf,
}

impl MachineKey {
    /// Load the key, or make and save one on first use.
    ///
    /// # Errors
    /// Fails when the file cannot be read or written, or is not 32 bytes.
    pub fn load_or_create() -> Result<Self, VivariumError> {
        let dir = keys_dir()?;
        let path = dir.join("machine.ed25519");
        if path.exists() {
            refuse_open_modes(&dir, &path)?;
            let bytes = fs::read(&path)?;
            let seed: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| err(format!("{} must hold 32 bytes", path.display())))?;
            return Ok(Self {
                signing_key: SigningKey::from_bytes(&seed),
                path,
            });
        }
        private_dir(&dir)?;
        let signing_key = SigningKey::generate(&mut OsRng);
        private_write(&path, &signing_key.to_bytes())?;
        Ok(Self { signing_key, path })
    }

    /// Where the key file is.
    #[must_use]
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Base64 of the raw 32-byte public key.
    #[must_use]
    pub fn public_key(&self) -> String {
        b64().encode(self.signing_key.verifying_key().as_bytes())
    }

    /// `SHA256:<base64 no padding>`, as ssh prints fingerprints.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let digest = Sha256::digest(self.signing_key.verifying_key().as_bytes());
        format!(
            "SHA256:{}",
            base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
        )
    }

    fn sign(&self, nonce: &[u8]) -> String {
        let mut message = MESSAGE_PREFIX.to_vec();
        message.extend_from_slice(nonce);
        b64().encode(self.signing_key.sign(&message).to_bytes())
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn post_json(url: &str, body: &Value) -> Result<Value, VivariumError> {
    let raw = crate::sync::http::send_raw(
        "POST",
        url,
        &[("Content-Type", "application/json".to_string())],
        Some(serde_json::to_vec(body).map_err(|e| err(e.to_string()))?),
    )?;
    let value: Value = serde_json::from_slice(&raw.body).unwrap_or(Value::Null);
    if raw.status >= 300 {
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("request failed");
        return Err(err(format!("{url}: {} {message}", raw.status)));
    }
    Ok(value)
}

/// A session token for `server`, signing in when there is no live cached
/// one. `as_host` asks for the host principal.
///
/// # Errors
/// Fails when the key cannot be loaded or the server refuses the sign-in.
pub fn sign_in(server: &str, as_host: bool) -> Result<String, VivariumError> {
    let server = server.trim_end_matches('/');
    let key = MachineKey::load_or_create()?;
    let sessions = keys_dir()?.join("sessions");
    let cache = cache_path(server, as_host, &key)?;
    if let Ok(text) = fs::read_to_string(&cache)
        && let Ok(cached) = serde_json::from_str::<Value>(&text)
        && cached["expires_at"].as_u64().unwrap_or(0) > now() + 60
        && let Some(token) = cached["token"].as_str()
    {
        return Ok(token.to_string());
    }
    let challenge = post_json(
        &format!("{server}/v1/auth/machine/challenge"),
        &json!({ "public_key": key.public_key() }),
    )?;
    let nonce = b64()
        .decode(
            challenge["nonce"]
                .as_str()
                .ok_or_else(|| err("challenge has no nonce"))?,
        )
        .map_err(|e| err(format!("challenge nonce is not base64: {e}")))?;
    let verified = post_json(
        &format!("{server}/v1/auth/machine/verify"),
        &json!({
            "challenge_id": challenge["challenge_id"],
            "signature": key.sign(&nonce),
            "as_host": as_host,
        }),
    )?;
    let token = verified["token"]
        .as_str()
        .ok_or_else(|| err("verify returned no token"))?
        .to_string();
    let expires_at = now() + verified["expires_in"].as_u64().unwrap_or(0);
    private_dir(&sessions)?;
    private_write(
        &cache,
        serde_json::to_string(&json!({ "token": token, "expires_at": expires_at }))
            .map_err(|e| err(e.to_string()))?
            .as_bytes(),
    )?;
    Ok(token)
}

fn cache_path(server: &str, as_host: bool, key: &MachineKey) -> Result<PathBuf, VivariumError> {
    let tag = hex::encode(Sha256::digest(
        format!("{server}\n{as_host}\n{}", key.public_key()).as_bytes(),
    ));
    Ok(keys_dir()?
        .join("sessions")
        .join(format!("{}.json", &tag[..16])))
}

/// Drop the cached session for `server` (the server refused it, for
/// example after a restart), so the next [`sign_in`] signs in afresh.
///
/// # Errors
/// Fails when the key cannot be loaded.
pub fn forget(server: &str, as_host: bool) -> Result<(), VivariumError> {
    let server = server.trim_end_matches('/');
    let key = MachineKey::load_or_create()?;
    let path = cache_path(server, as_host, &key)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        // No cached session is the state this function asks for.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(err(format!("cannot remove {}: {e}", path.display()))),
    }
}

/// Whether an error text reads as a refused bearer.
#[must_use]
pub fn is_refused_bearer(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("401") || lower.contains("unauthorized")
}
