//! ED25519 chat identity, minted lazily at `.vivi/chat/identity.ed25519`.

use std::fs;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use ed25519_dalek::SigningKey;
use rand_core::OsRng;
use sha2::{Digest, Sha256};

use crate::chat::ensure_chat_dir;
use crate::error::VivariumError;

const IDENTITY_FILE: &str = "identity.ed25519";

/// Hex characters kept in the short fingerprint (first half of the SHA-256
/// digest of the public key).
const FINGERPRINT_LEN: usize = 16;

/// The per-mailspace chat keypair. Seats bind to this key; the fingerprint is
/// the trust label shown at join and read.
#[derive(Debug, Clone)]
pub struct ChatIdentity {
    signing_key: SigningKey,
}

impl ChatIdentity {
    /// Load the keypair from its 32-byte seed file, or mint and persist one
    /// (mode 0600) on first use.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read, does not hold 32 bytes,
    /// or cannot be written.
    pub fn ensure(dir: &Path) -> Result<Self, VivariumError> {
        ensure_chat_dir(dir)?;
        let path = dir.join(IDENTITY_FILE);
        if path.exists() {
            return Self::load(&path);
        }
        let identity = Self {
            signing_key: SigningKey::generate(&mut OsRng),
        };
        fs::write(&path, identity.signing_key.to_bytes())?;
        Self::restrict_mode(&path)?;
        Ok(identity)
    }

    /// Load the keypair from a 32-byte seed file.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or is not 32 bytes.
    pub fn load(path: &Path) -> Result<Self, VivariumError> {
        let bytes = fs::read(path)?;
        let seed: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
            VivariumError::Message(format!(
                "chat identity file must hold 32 bytes: {}",
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

    /// Public key as an SPKI PEM (openssl `BEGIN PUBLIC KEY` shape), matching
    /// the frozen wire contract. ED25519 SPKI DER is a fixed 12-byte prefix
    /// followed by the 32-byte key.
    #[must_use]
    pub fn public_key_pem(&self) -> String {
        const ED25519_SPKI_PREFIX: &[u8] = &[
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        let mut der = ED25519_SPKI_PREFIX.to_vec();
        der.extend_from_slice(self.signing_key.verifying_key().as_bytes());
        let body = crate::chat::b64::encode(&der);
        format!("-----BEGIN PUBLIC KEY-----\n{body}\n-----END PUBLIC KEY-----")
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
}

#[cfg(test)]
#[path = "identity_test.rs"]
mod tests;
