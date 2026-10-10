//! Per-project admission registry (phase-03 decided item 8: admission by
//! introduction). Mirrors `IdentityRegistry` persistence: one record per
//! admitted name at `DATA_DIR/projects/<project_id>/admissions.json` (0600
//! atomic writes, lock-under-call). `postmaster` is never admissible.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::auth::{canonical_spki_hex, parse_verifying_key, rfc3339};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdmissionRecord {
    pub name: String,
    /// Canonical SPKI DER hex of the admitted key.
    pub spki_der: String,
    /// Admitting member's name, or `creator` for the bootstrap admission.
    pub admitted_by: String,
    pub admitted_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AdmissionsFile {
    pub admissions: Vec<AdmissionRecord>,
}

#[derive(Debug)]
pub enum AdmissionError {
    InvalidKey,
    InvalidName,
    ReservedName,
    Conflict(String),
    Storage(String),
}

/// Same rules as the identity validator in `identities.rs` (names double as
/// mail local-parts); duplicated here so admission does not depend on that
/// module's internals.
fn validate_name(name: &str) -> Result<String, AdmissionError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return Err(AdmissionError::InvalidName);
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(AdmissionError::InvalidName);
    }
    if trimmed.eq_ignore_ascii_case("postmaster") {
        return Err(AdmissionError::ReservedName);
    }
    Ok(trimmed.to_owned())
}

#[derive(Debug, Clone)]
pub struct AdmissionRegistry {
    lock: Arc<Mutex<()>>,
    root: PathBuf,
}

impl AdmissionRegistry {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            lock: Arc::new(Mutex::new(())),
            root: data_dir.join("projects"),
        }
    }

    fn admissions_path(&self, project_id: &str) -> PathBuf {
        self.root.join(project_id).join("admissions.json")
    }

    fn load(&self, project_id: &str) -> AdmissionsFile {
        std::fs::read(self.admissions_path(project_id))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Atomic 0600 persistence via temp file + rename.
    fn save(&self, project_id: &str, file: &AdmissionsFile) -> Result<(), String> {
        let bytes = serde_json::to_vec(file)
            .map_err(|e| format!("failed to encode admissions.json: {e}"))?;
        let path = self.admissions_path(project_id);
        let tmp = path.with_extension("json.tmp");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to prepare admissions dir: {e}"))?;
        }
        write_private(&tmp, &bytes).map_err(|e| format!("failed to write admissions.json: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("failed to place admissions.json: {e}"))
    }

    /// True when the exact name is admitted in this project.
    pub fn is_admitted(&self, project_id: &str, name: &str) -> bool {
        let _guard = self.lock();
        self.load(project_id)
            .admissions
            .iter()
            .any(|record| record.name == name)
    }

    /// Admit `name` bound to `public_key_pem`, attributed to `voucher`.
    /// Same name plus same key is an idempotent no-op; the same name with a
    /// different key conflicts; one key may hold several names.
    pub fn admit(
        &self,
        project_id: &str,
        name: &str,
        public_key_pem: &str,
        voucher: &str,
    ) -> Result<(), AdmissionError> {
        let _guard = self.lock();
        let key = parse_verifying_key(public_key_pem).map_err(|_| AdmissionError::InvalidKey)?;
        let spki_der = canonical_spki_hex(&key).map_err(|_| AdmissionError::InvalidKey)?;
        let name = validate_name(name)?;
        let mut file = self.load(project_id);
        if let Some(existing) = file.admissions.iter().find(|a| a.name == name) {
            if existing.spki_der == spki_der {
                return Ok(());
            }
            return Err(AdmissionError::Conflict(format!(
                "name {name} is already admitted for a different key"
            )));
        }
        file.admissions.push(AdmissionRecord {
            name,
            spki_der,
            admitted_by: voucher.to_owned(),
            admitted_at: rfc3339(Utc::now()),
        });
        self.save(project_id, &file)
            .map_err(AdmissionError::Storage)
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
