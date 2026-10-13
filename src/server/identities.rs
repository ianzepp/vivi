//! Per-project identity registry: flat identity names bound to ED25519 keys,
//! persisted at `DATA_DIR/projects/<project_id>/identities.json` (0600), one
//! record per identity with the canonical SPKI DER hex and bearer-token
//! records stored only as SHA-256 digests plus expiry. Raw tokens never touch
//! disk. `postmaster` is reserved in every project (phase-03 item 9).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::auth::{
    AuthError, ChallengeBook, canonical_spki_hex, constant_time_eq, key_fingerprint, mint_token,
    mint_token_until, parse_verifying_key, rfc3339, sha256_hex, verify_signature,
    verifying_key_from_spki_hex,
};

pub const MAX_IDENTITY_LEN: usize = 64;

/// Length of a mailspace id prefix: the domain part of its addresses
/// (phase-03 item 11).
pub const MAILSPACE_PREFIX_LEN: usize = 8;

/// True for a canonical mailspace id: lowercase hyphenated 8-4-4-4-12 hex.
pub fn valid_mailspace_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => matches!(b, b'0'..=b'9' | b'a'..=b'f'),
        })
}

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
    /// The one mailspace this identity registered (canonical UUID). Absent
    /// on identities registered before addresses; such an identity has no
    /// prefix and cannot receive mail by prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailspace_id: Option<String>,
    /// Keyless chat participant (item 17). Confined to chat projects.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub guest: bool,
}

impl IdentityRecord {
    /// The address domain prefix bound to this identity, when it has one.
    pub fn mailspace_prefix(&self) -> Option<&str> {
        self.mailspace_id
            .as_deref()
            .and_then(|id| id.get(..MAILSPACE_PREFIX_LEN))
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct IdentitiesFile {
    pub identities: Vec<IdentityRecord>,
}

#[derive(Debug)]
pub enum RegisterError {
    InvalidKey,
    InvalidName,
    /// `mailspace_id` is not a canonical lowercase hyphenated UUID.
    InvalidMailspaceId,
    NameTaken,
    ReservedName,
    /// Another identity in the project already holds the id's prefix.
    PrefixTaken,
    /// Chat project is at the member cap.
    RoomFull,
    Storage(String),
}

#[derive(Debug)]
pub enum RefreshError {
    /// Unknown name, wrong key, or a bad, expired, or replayed challenge or
    /// signature; deliberately one case.
    Unauthorized,
    Storage(String),
}

/// Identity binding outcome returned to the client once.
#[derive(Debug, Clone, Serialize)]
pub struct IdentitySummary {
    pub name: String,
    pub fingerprint: String,
    /// Null for an identity registered before addresses.
    pub mailspace_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RegisterOutcome {
    pub name: String,
    pub token: String,
    pub expires_at: String,
    pub fingerprint: String,
    pub mailspace_id: String,
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
    /// The identity also binds `mailspace_id`; its prefix must be unused by
    /// every other identity in the project (the name is checked first).
    pub fn register(
        &self,
        project_id: &str,
        name: &str,
        public_key_pem: &str,
        mailspace_id: &str,
    ) -> Result<RegisterOutcome, RegisterError> {
        let _guard = self.lock();
        if !valid_mailspace_id(mailspace_id) {
            return Err(RegisterError::InvalidMailspaceId);
        }
        let key = parse_verifying_key(public_key_pem).map_err(|_| RegisterError::InvalidKey)?;
        let fingerprint = key_fingerprint(&key).map_err(|_| RegisterError::InvalidKey)?;
        let spki_der = canonical_spki_hex(&key).map_err(|_| RegisterError::InvalidKey)?;
        let mut file = self.load(project_id);
        let name = validate_name(name, &file.identities)?;
        let prefix = &mailspace_id[..MAILSPACE_PREFIX_LEN];
        if file
            .identities
            .iter()
            .any(|i| i.mailspace_prefix() == Some(prefix))
        {
            return Err(RegisterError::PrefixTaken);
        }
        let (token, record) = self.mint_record()?;
        let expires_at = record.expires_at.clone();
        file.identities.push(IdentityRecord {
            name: name.clone(),
            fingerprint: fingerprint.clone(),
            spki_der,
            tokens: vec![record],
            mailspace_id: Some(mailspace_id.to_owned()),
            guest: false,
        });
        self.save(project_id, &file)
            .map_err(RegisterError::Storage)?;
        Ok(RegisterOutcome {
            name,
            token,
            expires_at,
            fingerprint,
            mailspace_id: mailspace_id.to_owned(),
        })
    }

    /// Renew the bearer for a registered identity. The challenge is spent
    /// on its first use, must have been issued for this project and the
    /// fingerprint bound to `name`, and the signature must verify under the
    /// key registered for that name. Earlier unexpired tokens stay valid.
    pub fn refresh_token(
        &self,
        project_id: &str,
        name: &str,
        challenge_id: &str,
        signature_base64: &str,
        book: &ChallengeBook,
    ) -> Result<(String, String), RefreshError> {
        let _guard = self.lock();
        let (_, fingerprint) = book.peek(challenge_id).ok_or(RefreshError::Unauthorized)?;
        let challenge = book
            .consume(project_id, challenge_id, &fingerprint)
            .map_err(|_| RefreshError::Unauthorized)?;
        let mut file = self.load(project_id);
        let identity = file
            .identities
            .iter_mut()
            .find(|i| i.name == name && i.fingerprint == fingerprint)
            .ok_or(RefreshError::Unauthorized)?;
        let key = verifying_key_from_spki_hex(&identity.spki_der)
            .map_err(|_| RefreshError::Unauthorized)?;
        if !verify_signature(&key, &challenge, signature_base64) {
            return Err(RefreshError::Unauthorized);
        }
        let (token, record) = self.mint_record().map_err(|err| match err {
            RegisterError::Storage(message) => RefreshError::Storage(message),
            _ => RefreshError::Storage("token mint failed".into()),
        })?;
        identity.tokens.push(record.clone());
        self.save(project_id, &file)
            .map_err(RefreshError::Storage)?;
        Ok((token, record.expires_at))
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
                mailspace_id: identity.mailspace_id,
            })
            .collect();
        identities.sort_by(|a, b| a.name.cmp(&b.name));
        identities
    }

    /// The registered record for `name`, if any.
    pub fn by_name(&self, project_id: &str, name: &str) -> Option<IdentityRecord> {
        let _guard = self.lock();
        self.load(project_id)
            .identities
            .into_iter()
            .find(|identity| identity.name == name)
    }

    /// Keyless chat join: a name plus a server-minted bearer that lasts
    /// until `expires_at`. The server mints a mailspace id so the guest has
    /// a From address. Confined to chat projects by the rooms API.
    pub fn register_guest(
        &self,
        project_id: &str,
        name: &str,
        expires_at: chrono::DateTime<chrono::Utc>,
        member_cap: usize,
    ) -> Result<RegisterOutcome, RegisterError> {
        let _guard = self.lock();
        let mut file = self.load(project_id);
        if file.identities.len() >= member_cap {
            return Err(RegisterError::RoomFull);
        }
        let name = validate_name(name, &file.identities)?;
        let mailspace_id = mint_guest_mailspace_id(&file.identities)?;
        let minted = mint_token_until(expires_at).map_err(|err| match err {
            AuthError::Entropy => RegisterError::Storage("entropy unavailable".into()),
            _ => RegisterError::Storage("token mint failed".into()),
        })?;
        let record = token_record(minted.digest.clone(), minted.expires_at);
        let expires_at = record.expires_at.clone();
        file.identities.push(IdentityRecord {
            name: name.clone(),
            fingerprint: format!("guest:{name}"),
            spki_der: String::new(),
            tokens: vec![record],
            mailspace_id: Some(mailspace_id.clone()),
            guest: true,
        });
        self.save(project_id, &file)
            .map_err(RegisterError::Storage)?;
        Ok(RegisterOutcome {
            name: name.clone(),
            token: minted.token,
            expires_at,
            fingerprint: format!("guest:{name}"),
            mailspace_id,
        })
    }

    /// How many identities are registered in the project.
    pub fn member_count(&self, project_id: &str) -> usize {
        let _guard = self.lock();
        self.load(project_id).identities.len()
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

/// UUID v4 used as a guest's mailspace id so they have an address prefix.
fn mint_guest_mailspace_id(identities: &[IdentityRecord]) -> Result<String, RegisterError> {
    for _ in 0..8 {
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw)
            .map_err(|_| RegisterError::Storage("entropy unavailable".into()))?;
        raw[6] = (raw[6] & 0x0f) | 0x40;
        raw[8] = (raw[8] & 0x3f) | 0x80;
        let id = format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            raw[0],
            raw[1],
            raw[2],
            raw[3],
            raw[4],
            raw[5],
            raw[6],
            raw[7],
            raw[8],
            raw[9],
            raw[10],
            raw[11],
            raw[12],
            raw[13],
            raw[14],
            raw[15]
        );
        let prefix = &id[..MAILSPACE_PREFIX_LEN];
        if !identities
            .iter()
            .any(|identity| identity.mailspace_prefix() == Some(prefix))
        {
            return Ok(id);
        }
    }
    Err(RegisterError::PrefixTaken)
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
