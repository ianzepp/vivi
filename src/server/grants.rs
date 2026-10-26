//! Server-level API keys and grants for the one `/v1/mcp` endpoint
//! (wave 3 D2): a single key store at `DATA_DIR/keys.json` shared by
//! every project, replacing the per-project registry in `keys.rs` once
//! the route cutover lands. A principal owns keys (SHA-256 digests only;
//! a raw secret exists once, at mint) and grants. A host principal
//! (`host: true`, such as the `HOST_OPERATOR_TOKEN` bearer) carries no
//! project grants and reaches only host tools. Authorization rules are
//! re-checked per call by the route layer; this module stores and
//! verifies. [`retire_legacy_project_keys`] moves the old per-project
//! key files aside for the boot wiring; nothing calls it yet.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use crate::server::auth::{AuthError, constant_time_eq, mint_token, rfc3339, sha256_hex};

const KEY_PREFIX: &str = "vivi_";
const KEYS_FILE: &str = "keys.json";
const MAX_KEYS_PER_PRINCIPAL: usize = 32;
const MAX_LABEL: usize = 64;
const MAX_NAME: usize = 64;
const MAX_PROJECT: usize = 128;
const KEY_ID_BYTES: usize = 8;

/// What a grant allows on one project. The operator RULES around a level
/// (host's own member, acting role kind operator) are re-checked by the
/// route per call; only the level is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantLevel {
    Member,
    Operator,
}

/// One project grant held by a principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantRecord {
    pub project_id: String,
    /// The registered identity the grant's member is.
    pub member: String,
    /// The vivi identity the key acts as on this project, when one was
    /// named. Absent means the member identity itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    pub level: GrantLevel,
}

/// A stored server key. The raw secret was shown once at mint; only its
/// SHA-256 digest is persisted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerKeyRecord {
    pub key_id: String,
    pub label: String,
    pub digest: String,
    pub created_by: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
}

/// A principal and everything it holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalRecord {
    /// Stable principal id. A self-service mint with no acting identity
    /// is `<member>@<project_id>`; one that names an identity is
    /// `<member>.<role>@<project_id>`, so a later key does not resolve
    /// the first grant. A host-operator token uses a fixed name such as
    /// `host`. Two derivations never share a principal:
    /// [`ServerKeyStore::mint_derived`] refuses the second to arrive.
    pub principal: String,
    #[serde(default)]
    pub host: bool,
    #[serde(default)]
    pub keys: Vec<ServerKeyRecord>,
    #[serde(default)]
    pub grants: Vec<GrantRecord>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerKeysFile {
    #[serde(default)]
    principals: Vec<PrincipalRecord>,
}

/// A key as listed: everything stored except nothing — digests only,
/// never a raw secret.
#[derive(Debug, Clone, Serialize)]
pub struct ServerKeySummary {
    pub key_id: String,
    pub label: String,
    pub digest: String,
    pub created_by: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
}

/// A principal as listed: id, host flag, its keys and grants.
#[derive(Debug, Clone, Serialize)]
pub struct PrincipalSummary {
    pub principal: String,
    pub host: bool,
    pub keys: Vec<ServerKeySummary>,
    pub grants: Vec<GrantRecord>,
}

/// What a live server key resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedServerKey {
    pub principal: String,
    /// The matched key, for rate limiting and audit lines.
    pub key_id: String,
    pub host: bool,
    pub grants: Vec<GrantRecord>,
}

#[derive(Debug, Clone)]
pub struct MintedServerKey {
    pub principal: String,
    pub key_id: String,
    /// The raw secret; returned once, never stored.
    pub key: String,
    pub label: String,
    pub created_by: String,
    pub created_at: String,
    pub host: bool,
    pub grants: Vec<GrantRecord>,
}

#[derive(Debug)]
pub enum GrantsError {
    InvalidPrincipal,
    InvalidLabel,
    InvalidGrant,
    /// A host principal was minted with project grants.
    HostWithGrants,
    /// A principal was minted again with a different `host` flag.
    HostMismatch,
    /// A self-service mint derived a principal that already exists under
    /// a different member/identity derivation. The message names both
    /// derivations; it is a refusal, never a silent upsert.
    PrincipalCollision(String),
    LimitReached,
    NotFound,
    Storage(String),
}

struct IndexEntry {
    digest: String,
    principal: String,
    key_id: String,
    revoked: bool,
}

struct StoreState {
    file: ServerKeysFile,
    index: Vec<IndexEntry>,
}

/// The one server key store at `DATA_DIR/keys.json`. Clone-safe: every
/// method locks one shared state, and the digest index is rebuilt on
/// every write.
#[derive(Clone)]
pub struct ServerKeyStore {
    state: Arc<Mutex<StoreState>>,
    path: PathBuf,
}

impl ServerKeyStore {
    /// Load the store (an absent file is an empty store; an undecodable
    /// file is an error, never an empty store) and build the digest
    /// index.
    pub fn new(data_dir: &Path) -> Result<Self, GrantsError> {
        let path = data_dir.join(KEYS_FILE);
        let file = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                GrantsError::Storage(format!("failed to parse {}: {e}", path.display()))
            })?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => ServerKeysFile::default(),
            Err(err) => {
                return Err(GrantsError::Storage(format!(
                    "failed to read {}: {err}",
                    path.display()
                )));
            }
        };
        let index = build_index(&file);
        Ok(Self {
            state: Arc::new(Mutex::new(StoreState { file, index })),
            path,
        })
    }

    fn state(&self) -> MutexGuard<'_, StoreState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(path: &Path, file: &ServerKeysFile) -> Result<(), String> {
        let bytes =
            serde_json::to_vec(file).map_err(|e| format!("failed to encode keys.json: {e}"))?;
        let tmp = path.with_extension("json.tmp");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to prepare keys.json dir: {e}"))?;
        }
        write_private(&tmp, &bytes).map_err(|e| format!("failed to write keys.json: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("failed to place keys.json: {e}"))
    }

    /// Mint a key for `principal`, creating the principal when absent.
    /// On an existing principal the `host` flag must match and grants not
    /// already held are added (an exact re-mint adds none). A host
    /// principal must carry no project grants. The raw secret is
    /// returned once; only its digest is persisted.
    pub fn mint(
        &self,
        principal: &str,
        host: bool,
        grants: Vec<GrantRecord>,
        label: &str,
        created_by: &str,
    ) -> Result<MintedServerKey, GrantsError> {
        let principal = validate_principal(principal)?;
        let label = validate_label(label)?;
        let created_by = validate_member(created_by)?;
        let mut grants = grants;
        for grant in &mut grants {
            validate_grant(grant)?;
        }
        if host && !grants.is_empty() {
            return Err(GrantsError::HostWithGrants);
        }
        let mut state = self.state();
        self.mint_locked(&mut state, principal, host, grants, label, &created_by)
    }

    /// The mint itself, under the caller's lock.
    fn mint_locked(
        &self,
        state: &mut StoreState,
        principal: String,
        host: bool,
        grants: Vec<GrantRecord>,
        label: String,
        created_by: &str,
    ) -> Result<MintedServerKey, GrantsError> {
        let pos = match state
            .file
            .principals
            .iter()
            .position(|p| p.principal == principal)
        {
            Some(pos) => {
                let existing = &mut state.file.principals[pos];
                if existing.host != host {
                    return Err(GrantsError::HostMismatch);
                }
                let live = existing
                    .keys
                    .iter()
                    .filter(|key| key.revoked_at.is_none())
                    .count();
                if live >= MAX_KEYS_PER_PRINCIPAL {
                    return Err(GrantsError::LimitReached);
                }
                for grant in grants {
                    if !existing.grants.contains(&grant) {
                        existing.grants.push(grant);
                    }
                }
                pos
            }
            None => {
                state.file.principals.push(PrincipalRecord {
                    principal: principal.clone(),
                    host,
                    keys: Vec::new(),
                    grants,
                });
                state.file.principals.len() - 1
            }
        };
        let minted = mint_token().map_err(|err| match err {
            AuthError::Entropy => GrantsError::Storage("entropy unavailable".into()),
            _ => GrantsError::Storage("key mint failed".into()),
        })?;
        let key = format!("{KEY_PREFIX}{}", minted.token);
        let digest = sha256_hex(key.as_bytes());
        let key_id = mint_key_id().map_err(|err| match err {
            AuthError::Entropy => GrantsError::Storage("entropy unavailable".into()),
            _ => GrantsError::Storage("key id mint failed".into()),
        })?;
        let created_at = rfc3339(chrono::Utc::now());
        state.file.principals[pos].keys.push(ServerKeyRecord {
            key_id: key_id.clone(),
            label: label.clone(),
            digest,
            created_by: created_by.to_owned(),
            created_at: created_at.clone(),
            revoked_at: None,
        });
        let record = &state.file.principals[pos];
        let out = MintedServerKey {
            grants: record.grants.clone(),
            host: record.host,
            principal: record.principal.clone(),
            key_id,
            key,
            label,
            created_by: created_by.to_owned(),
            created_at,
        };
        Self::save(&self.path, &state.file).map_err(GrantsError::Storage)?;
        state.index = build_index(&state.file);
        Ok(out)
    }

    /// Mint the self-service key for `member` on `project_id` — the
    /// grant `level` the caller derived, acting as `acting` when one is
    /// named — on the derived principal (see [`PrincipalRecord`]). A
    /// derivation never silently adopts an existing principal: when that
    /// principal already exists, every grant it holds must carry the same
    /// member and acting identity (a re-mint), else the mint is refused —
    /// two derivations on one principal would let the later key resolve
    /// the first grant and act as another member. Member names allow `.`,
    /// so `creator` acting as `ops` and a member literally named
    /// `creator.ops` derive the same principal; the second derivation to
    /// arrive loses.
    pub fn mint_derived(
        &self,
        project_id: &str,
        member: &str,
        acting: Option<&str>,
        level: GrantLevel,
        label: &str,
    ) -> Result<MintedServerKey, GrantsError> {
        let label = validate_label(label)?;
        let member = validate_member(member)?;
        let acting = acting.map(validate_member).transpose()?;
        let mut grant = GrantRecord {
            project_id: project_id.to_owned(),
            member: member.clone(),
            identity: acting.clone(),
            level,
        };
        validate_grant(&mut grant)?;
        let principal = self_service_principal(&grant.project_id, &member, acting.as_deref());
        let principal = validate_principal(&principal)?;
        let mut state = self.state();
        if let Some(existing) = state
            .file
            .principals
            .iter()
            .find(|p| p.principal == principal)
        {
            let deviating: Vec<&GrantRecord> = existing
                .grants
                .iter()
                .filter(|grant| {
                    (grant.member.as_str(), grant.identity.as_deref())
                        != (member.as_str(), acting.as_deref())
                })
                .collect();
            if !deviating.is_empty() {
                let held = deviating
                    .iter()
                    .map(|grant| describe_derivation(&grant.member, grant.identity.as_deref()))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(GrantsError::PrincipalCollision(format!(
                    "principal '{principal}' is already derived from {held}; this mint derives \
                     it from {}. A principal name cannot be shared across member/identity \
                     derivations",
                    describe_derivation(&member, acting.as_deref())
                )));
            }
        }
        self.mint_locked(&mut state, principal, false, vec![grant], label, &member)
    }

    /// The principal that holds `key_id` (live or revoked) and the project
    /// ids of its grants — where the per-project mirror of a self-service
    /// key can live. `None` when no principal holds that key id.
    pub fn holder_of_key(&self, key_id: &str) -> Option<(String, Vec<String>)> {
        let state = self.state();
        let principal = state
            .file
            .principals
            .iter()
            .find(|p| p.keys.iter().any(|key| key.key_id == key_id))?;
        Some((
            principal.principal.clone(),
            principal
                .grants
                .iter()
                .map(|grant| grant.project_id.clone())
                .collect(),
        ))
    }

    /// The key ids of `principal` (revoked and live), normalized as
    /// [`Self::remove_grant`] normalizes, `None` for an unknown or
    /// invalid principal.
    pub fn key_ids_of_principal(&self, principal: &str) -> Option<Vec<String>> {
        let principal = validate_principal(principal).ok()?;
        let state = self.state();
        Some(
            state
                .file
                .principals
                .iter()
                .find(|p| p.principal == principal)?
                .keys
                .iter()
                .map(|key| key.key_id.clone())
                .collect(),
        )
    }

    /// List principals, their keys (digests only) and their grants.
    pub fn list(&self) -> Vec<PrincipalSummary> {
        let state = self.state();
        state
            .file
            .principals
            .iter()
            .map(|principal| PrincipalSummary {
                principal: principal.principal.clone(),
                host: principal.host,
                keys: principal
                    .keys
                    .iter()
                    .map(|key| ServerKeySummary {
                        key_id: key.key_id.clone(),
                        label: key.label.clone(),
                        digest: key.digest.clone(),
                        created_by: key.created_by.clone(),
                        created_at: key.created_at.clone(),
                        revoked_at: key.revoked_at.clone(),
                    })
                    .collect(),
                grants: principal.grants.clone(),
            })
            .collect()
    }

    /// Revoke one key by id, on any principal. Already-revoked is
    /// success; an unknown key id is `NotFound`.
    pub fn revoke_key(&self, key_id: &str) -> Result<(), GrantsError> {
        let mut state = self.state();
        let now = rfc3339(chrono::Utc::now());
        let mut found = false;
        let mut changed = false;
        for principal in &mut state.file.principals {
            for key in &mut principal.keys {
                if key.key_id == key_id {
                    found = true;
                    if key.revoked_at.is_none() {
                        key.revoked_at = Some(now.clone());
                        changed = true;
                    }
                }
            }
        }
        if !found {
            return Err(GrantsError::NotFound);
        }
        if changed {
            Self::save(&self.path, &state.file).map_err(GrantsError::Storage)?;
            state.index = build_index(&state.file);
        }
        Ok(())
    }

    /// Add one grant to an existing principal (idempotent: a grant already
    /// held changes nothing). A host principal accepts no grants. An
    /// unknown principal is `NotFound`.
    pub fn add_grant(&self, principal: &str, grant: GrantRecord) -> Result<(), GrantsError> {
        let principal = validate_principal(principal)?;
        let mut grant = grant;
        validate_grant(&mut grant)?;
        let mut state = self.state();
        let Some(record) = state
            .file
            .principals
            .iter_mut()
            .find(|p| p.principal == principal)
        else {
            return Err(GrantsError::NotFound);
        };
        if record.host {
            return Err(GrantsError::HostWithGrants);
        }
        if record.grants.contains(&grant) {
            return Ok(());
        }
        record.grants.push(grant);
        Self::save(&self.path, &state.file).map_err(GrantsError::Storage)?;
        state.index = build_index(&state.file);
        Ok(())
    }

    /// Remove the grant on `project_id` from `principal`. An unknown
    /// principal, or one that holds no grant there, is `NotFound`.
    pub fn remove_grant(&self, principal: &str, project_id: &str) -> Result<(), GrantsError> {
        let principal = validate_principal(principal)?;
        let project_id = project_id.trim();
        if project_id.is_empty() || project_id.len() > MAX_PROJECT {
            return Err(GrantsError::NotFound);
        }
        let mut state = self.state();
        let Some(record) = state
            .file
            .principals
            .iter_mut()
            .find(|p| p.principal == principal)
        else {
            return Err(GrantsError::NotFound);
        };
        let before = record.grants.len();
        record.grants.retain(|grant| grant.project_id != project_id);
        if record.grants.len() == before {
            return Err(GrantsError::NotFound);
        }
        Self::save(&self.path, &state.file).map_err(GrantsError::Storage)?;
        state.index = build_index(&state.file);
        Ok(())
    }

    /// Revoke every key of `principal` (the principal and its grants
    /// stay on disk, unusable). Returns how many live keys died; an
    /// unknown principal is `NotFound`.
    pub fn revoke_principal(&self, principal: &str) -> Result<usize, GrantsError> {
        let mut state = self.state();
        let Some(record) = state
            .file
            .principals
            .iter_mut()
            .find(|p| p.principal == principal)
        else {
            return Err(GrantsError::NotFound);
        };
        let now = rfc3339(chrono::Utc::now());
        let revoked = record
            .keys
            .iter_mut()
            .filter(|key| key.revoked_at.is_none())
            .map(|key| {
                key.revoked_at = Some(now.clone());
            })
            .count();
        if revoked > 0 {
            Self::save(&self.path, &state.file).map_err(GrantsError::Storage)?;
            state.index = build_index(&state.file);
        }
        Ok(revoked)
    }

    /// The principal (id, host flag) and grants a live key resolves to,
    /// or `None` for an unknown or revoked secret. The presented secret
    /// is hashed first; the digest comparison is constant-time.
    pub fn verify(&self, presented: &str) -> Option<VerifiedServerKey> {
        let state = self.state();
        let digest = sha256_hex(presented.as_bytes());
        let entry = state
            .index
            .iter()
            .find(|entry| !entry.revoked && constant_time_eq(&digest, &entry.digest))?;
        let principal = state
            .file
            .principals
            .iter()
            .find(|p| p.principal == entry.principal)?;
        Some(VerifiedServerKey {
            principal: principal.principal.clone(),
            key_id: entry.key_id.clone(),
            host: principal.host,
            grants: principal.grants.clone(),
        })
    }
}

fn build_index(file: &ServerKeysFile) -> Vec<IndexEntry> {
    let mut index = Vec::new();
    for principal in &file.principals {
        for key in &principal.keys {
            index.push(IndexEntry {
                digest: key.digest.clone(),
                principal: principal.principal.clone(),
                key_id: key.key_id.clone(),
                revoked: key.revoked_at.is_some(),
            });
        }
    }
    index
}

fn validate_principal(principal: &str) -> Result<String, GrantsError> {
    let trimmed = principal.trim();
    let valid = !trimmed.is_empty()
        && trimmed.len() <= MAX_NAME
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@'));
    if valid {
        Ok(trimmed.to_owned())
    } else {
        Err(GrantsError::InvalidPrincipal)
    }
}

/// The principal a self-service mint derives from `member` on
/// `project_id` ([`PrincipalRecord`]'s naming scheme).
fn self_service_principal(project_id: &str, member: &str, acting: Option<&str>) -> String {
    match acting {
        Some(role) => format!("{member}.{role}@{project_id}"),
        None => format!("{member}@{project_id}"),
    }
}

/// How one member/identity derivation reads in a collision refusal.
fn describe_derivation(member: &str, identity: Option<&str>) -> String {
    match identity {
        Some(role) => format!("member '{member}' acting as '{role}'"),
        None => format!("member '{member}' with no acting identity"),
    }
}

fn validate_label(label: &str) -> Result<String, GrantsError> {
    let trimmed = label.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_LABEL {
        return Err(GrantsError::InvalidLabel);
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(GrantsError::InvalidLabel);
    }
    Ok(trimmed.to_owned())
}

/// A vivi identity name: the mail local-part charset, 1-64 characters.
fn validate_member(member: &str) -> Result<String, GrantsError> {
    let trimmed = member.trim();
    let valid = !trimmed.is_empty()
        && trimmed.len() <= MAX_LABEL
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if valid {
        Ok(trimmed.to_owned())
    } else {
        Err(GrantsError::InvalidGrant)
    }
}

fn validate_grant(grant: &mut GrantRecord) -> Result<(), GrantsError> {
    let project_id = grant.project_id.trim();
    if project_id.is_empty()
        || project_id.len() > MAX_PROJECT
        || project_id.chars().any(|c| c.is_control())
    {
        return Err(GrantsError::InvalidGrant);
    }
    grant.project_id = project_id.to_owned();
    grant.member = validate_member(&grant.member)?;
    grant.identity = grant.identity.as_deref().map(validate_member).transpose()?;
    Ok(())
}

fn mint_key_id() -> Result<String, AuthError> {
    let mut raw = [0u8; KEY_ID_BYTES];
    getrandom::fill(&mut raw).map_err(|_| AuthError::Entropy)?;
    Ok(format!("key_{}", URL_SAFE_NO_PAD.encode(raw)))
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

#[derive(Debug, Deserialize)]
struct LegacyKeysFile {
    #[serde(default)]
    keys: Vec<serde_json::Value>,
}

/// Rename each pre-upgrade `DATA_DIR/projects/<id>/api_keys.json` to
/// `api_keys.json.retired` once per data directory, and report
/// `(project_id, dead_key_count)` per renamed project. The sweep records
/// `DATA_DIR/api_keys.retirement` and later boots leave live files alone:
/// a self-service mint after the sweep re-creates `api_keys.json` for
/// rooms auth and the letter wire, and a restart must not retire that
/// mirror. A project whose `.retired` name already exists is skipped
/// during the one sweep; so is a project with no live key file. An
/// unreadable file is renamed with a count of 0.
pub fn retire_legacy_project_keys(data_dir: &Path) -> Vec<(String, usize)> {
    let marker = data_dir.join("api_keys.retirement");
    if marker.is_file() {
        return Vec::new();
    }
    let retired = retire_legacy_project_keys_once(data_dir);
    if let Err(err) = std::fs::create_dir_all(data_dir) {
        tracing::warn!("failed to record api key retirement: {err}");
        return retired;
    }
    if let Err(err) = std::fs::write(&marker, b"retired\n") {
        tracing::warn!("failed to record api key retirement: {err}");
    }
    retired
}

fn retire_legacy_project_keys_once(data_dir: &Path) -> Vec<(String, usize)> {
    let root = data_dir.join("projects");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut projects: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    projects.sort();
    let mut retired = Vec::new();
    for project in projects {
        let dir = root.join(&project);
        let live = dir.join("api_keys.json");
        let gone = dir.join("api_keys.json.retired");
        if !live.is_file() || gone.exists() {
            continue;
        }
        let dead = std::fs::read(&live)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<LegacyKeysFile>(&bytes).ok())
            .map(|file| file.keys.len())
            .unwrap_or(0);
        match std::fs::rename(&live, &gone) {
            Ok(()) => retired.push((project, dead)),
            Err(err) => tracing::warn!("failed to retire keys for {project}: {err}"),
        }
    }
    retired
}
