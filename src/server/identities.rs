//! Per-project identity registry: flat identity names bound to ED25519 keys,
//! persisted at `DATA_DIR/projects/<project_id>/identities.json` (0600), one
//! record per identity with the canonical SPKI DER hex and bearer-token
//! records stored only as SHA-256 digests plus expiry. Raw tokens never touch
//! disk. `postmaster` is reserved in every project (phase-03 item 9).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::auth::{
    AuthError, canonical_spki_hex, constant_time_eq, key_fingerprint, mint_token,
    parse_verifying_key, rfc3339, sha256_hex,
};

pub const MAX_IDENTITY_LEN: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRecord {
    pub digest: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityRecord {
    pub name: String,
    pub fingerprint: String,
    /// Canonical SPKI DER, hex. Public key material; identifies the key that
    /// registered the name. Raw tokens are never persisted.
    pub spki_der: String,
    pub tokens: Vec<TokenRecord>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct IdentitiesFile {
    pub identities: Vec<IdentityRecord>,
}

#[derive(Debug)]
pub enum RegisterError {
    InvalidKey,
    InvalidName,
    NameTaken,
    ReservedName,
    Storage(String),
}

/// Identity binding outcome returned to the client once.
#[derive(Debug, Clone, Serialize)]
pub struct IdentitySummary {
    pub name: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone)]
pub struct RegisterOutcome {
    pub name: String,
    pub token: String,
    pub expires_at: String,
    pub fingerprint: String,
}

#[derive(Clone)]
pub struct IdentityRegistry {
    lock: Arc<Mutex<()>>,
    root: PathBuf,
}

fn token_record(digest: String, expires_at: chrono::DateTime<chrono::Utc>) -> TokenRecord {
    TokenRecord {
        digest,
        expires_at: rfc3339(expires_at),
    }
}

fn not_expired(record: &TokenRecord) -> bool {
    match chrono::DateTime::parse_from_rfc3339(&record.expires_at) {
        Ok(expires_at) => expires_at.with_timezone(&chrono::Utc) > chrono::Utc::now(),
        Err(_) => false,
    }
}

fn validate_name(name: &str, identities: &[IdentityRecord]) -> Result<String, RegisterError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_IDENTITY_LEN {
        return Err(RegisterError::InvalidName);
    }
    // Identity names double as mail local-parts (phase-03 item 3): keep to
    // that charset.
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(RegisterError::InvalidName);
    }
    // Reserved administrative addressee; registrable never (item 9).
    if trimmed.eq_ignore_ascii_case("postmaster") {
        return Err(RegisterError::ReservedName);
    }
    if identities.iter().any(|i| i.name == trimmed) {
        return Err(RegisterError::NameTaken);
    }
    Ok(trimmed.to_owned())
}

impl IdentityRegistry {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            lock: Arc::new(Mutex::new(())),
            root: data_dir.join("projects"),
        }
    }

    fn identities_path(&self, project_id: &str) -> PathBuf {
        self.root.join(project_id).join("identities.json")
    }

    fn load(&self, project_id: &str) -> IdentitiesFile {
        std::fs::read(self.identities_path(project_id))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Atomic 0600 persistence via temp file + rename.
    fn save(&self, project_id: &str, file: &IdentitiesFile) -> Result<(), String> {
        let mut pruned = IdentitiesFile {
            identities: file.identities.clone(),
        };
        for identity in &mut pruned.identities {
            identity.tokens.retain(not_expired);
        }
        let bytes = serde_json::to_vec(&pruned)
            .map_err(|e| format!("failed to encode identities.json: {e}"))?;
        let path = self.identities_path(project_id);
        let tmp = path.with_extension("json.tmp");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to prepare identities dir: {e}"))?;
        }
        write_private(&tmp, &bytes).map_err(|e| format!("failed to write identities.json: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("failed to place identities.json: {e}"))
    }

    fn mint_record(&self) -> Result<(String, TokenRecord), RegisterError> {
        let minted = mint_token().map_err(|err| match err {
            AuthError::Entropy => RegisterError::Storage("entropy unavailable".into()),
            _ => RegisterError::Storage("token mint failed".into()),
        })?;
        let record = token_record(minted.digest.clone(), minted.expires_at);
        Ok((minted.token, record))
    }

    /// Bind a flat identity name to an ED25519 key: parse the PEM, store the
    /// key fingerprint and canonical SPKI hex, and mint a bearer token bound
    /// to the identity (raw form returned once; only its digest is stored).
    pub fn register(
        &self,
        project_id: &str,
        name: &str,
        public_key_pem: &str,
    ) -> Result<RegisterOutcome, RegisterError> {
        let _guard = self.lock();
        let key = parse_verifying_key(public_key_pem).map_err(|_| RegisterError::InvalidKey)?;
        let fingerprint = key_fingerprint(&key).map_err(|_| RegisterError::InvalidKey)?;
        let spki_der = canonical_spki_hex(&key).map_err(|_| RegisterError::InvalidKey)?;
        let mut file = self.load(project_id);
        let name = validate_name(name, &file.identities)?;
        let (token, record) = self.mint_record()?;
        let expires_at = record.expires_at.clone();
        file.identities.push(IdentityRecord {
            name: name.clone(),
            fingerprint: fingerprint.clone(),
            spki_der,
            tokens: vec![record],
        });
        self.save(project_id, &file)
            .map_err(RegisterError::Storage)?;
        Ok(RegisterOutcome {
            name,
            token,
            expires_at,
            fingerprint,
        })
    }

    /// List registered identities without exposing persisted public-key or token data.
    pub fn list(&self, project_id: &str) -> Vec<IdentitySummary> {
        let _guard = self.lock();
        let mut identities: Vec<_> = self
            .load(project_id)
            .identities
            .into_iter()
            .map(|identity| IdentitySummary {
                name: identity.name,
                fingerprint: identity.fingerprint,
            })
            .collect();
        identities.sort_by(|a, b| a.name.cmp(&b.name));
        identities
    }

    /// Resolve a presented bearer to its identity record. Digest comparison
    /// is constant-time; expired records never match; other projects fail
    /// closed.
    pub fn verify_bearer(&self, project_id: &str, presented: &str) -> Option<IdentityRecord> {
        let _guard = self.lock();
        let digest = sha256_hex(presented.as_bytes());
        self.load(project_id)
            .identities
            .into_iter()
            .find(|identity| {
                identity
                    .tokens
                    .iter()
                    .any(|record| not_expired(record) && constant_time_eq(&digest, &record.digest))
            })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}
