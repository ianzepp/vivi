//! The mailspace's ED25519 sync identity, minted lazily at
//! `.vivi/chat/identity.ed25519`.
//!
//! The directory is named for the `vivi chat` command this key was minted for
//! and is kept on purpose: a project server binds a name to the first key
//! admitted and has no way to re-key it, so moving the file would strand
//! every name already registered from an existing mailspace.

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use ed25519_dalek::SigningKey;
use rand_core::OsRng;
use sha2::{Digest, Sha256};

use crate::error::VivariumError;
use crate::sync::state::ensure_sync_dir;

const IDENTITY_DIR: &str = "chat";
const IDENTITY_FILE: &str = "identity.ed25519";

/// Hex characters kept in the short fingerprint (first half of the SHA-256
/// digest of the public key).
const FINGERPRINT_LEN: usize = 16;

/// The per-mailspace keypair. The project server binds a registered name to
/// this key; the fingerprint is the trust label shown at join and status.
#[derive(Debug, Clone)]
pub struct SyncIdentity {
    signing_key: SigningKey,
}

impl SyncIdentity {
    /// Load the keypair from its 32-byte seed file under `vivi_dir`, or mint
    /// and persist one (mode 0600) on first use.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read, does not hold 32 bytes,
    /// or cannot be written.
    pub fn ensure(vivi_dir: &Path) -> Result<Self, VivariumError> {
        ensure_sync_dir(&vivi_dir.join(IDENTITY_DIR))?;
        let path = identity_path(vivi_dir);
        if path.exists() {
            return Self::load(&path);
        }
        let identity = Self {
            signing_key: SigningKey::generate(&mut OsRng),
        };
        fs::write(&path, identity.signing_key.to_bytes())?;
        restrict_mode(&path)?;
        Ok(identity)
    }

    /// Load the 32-byte seed file at `path` (the format of
    /// `.vivi/chat/identity.ed25519`) without creating anything: no
    /// directory, no file. For a bridge member name plus key file there is
    /// no mailspace to mint into.
    ///
    /// # Errors
    /// Returns an error naming `path` when the file is missing, unreadable,
    /// or does not hold 32 bytes.
    pub fn from_key_file(path: &Path) -> Result<Self, VivariumError> {
        Self::load(path)
    }

    fn load(path: &Path) -> Result<Self, VivariumError> {
        let bytes = fs::read(path).map_err(|e| {
            VivariumError::Message(format!(
                "cannot read sync identity file {}: {e}",
                path.display()
            ))
        })?;
        let seed: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
            VivariumError::Message(format!(
                "sync identity file must hold 32 bytes: {}",
                path.display()
            ))
        })?;
        Ok(Self {
            signing_key: SigningKey::from_bytes(&seed),
        })
    }

    /// Short fingerprint: first 16 hex chars of SHA-256 of the public key.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let digest = Sha256::digest(self.signing_key.verifying_key().as_bytes());
        hex::encode(digest)[..FINGERPRINT_LEN].to_string()
    }

    /// Sign an exact byte string (the issued challenge bytes) and return the
    /// raw 64-byte signature.
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer;
        self.signing_key.sign(message).to_bytes()
    }

    /// Canonical SPKI DER bytes for the public key (the form the server
    /// fingerprints and persists per name).
    fn spki_der(&self) -> Vec<u8> {
        const ED25519_SPKI_PREFIX: &[u8] = &[
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        let mut der = ED25519_SPKI_PREFIX.to_vec();
        der.extend_from_slice(self.signing_key.verifying_key().as_bytes());
        der
    }

    /// Wire fingerprint: lowercase hex SHA-256 over the hex-encoded SPKI DER
    /// string, matching the server's canonical fingerprint (64 chars).
    #[must_use]
    pub fn wire_fingerprint(&self) -> String {
        let hex_der = hex::encode(self.spki_der());
        hex::encode(Sha256::digest(hex_der.as_bytes()))
    }

    /// Public key as an SPKI PEM (openssl `BEGIN PUBLIC KEY` shape), matching
    /// the wire contract.
    #[must_use]
    pub fn public_key_pem(&self) -> String {
        let body = crate::b64::encode(&self.spki_der());
        format!("-----BEGIN PUBLIC KEY-----\n{body}\n-----END PUBLIC KEY-----")
    }
}

fn identity_path(vivi_dir: &Path) -> PathBuf {
    vivi_dir.join(IDENTITY_DIR).join(IDENTITY_FILE)
}

#[cfg(unix)]
fn restrict_mode(path: &Path) -> Result<(), VivariumError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_mode(_path: &Path) -> Result<(), VivariumError> {
    Ok(())
}

#[cfg(test)]
#[path = "identity_test.rs"]
mod tests;
