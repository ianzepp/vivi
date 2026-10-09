//! At-rest encryption for room message content (VC-5): AES-256-GCM under
//! `STORE_ENCRYPTION_KEY` (standard base64 of 32 random bytes), a boot-time
//! canary verifier persisted beside the data, and 0600/0700 permission
//! hardening for room files.

use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, AeadCore, OsRng};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// Fixed plaintext sealed into the verifier file at first boot. A later boot
/// must decrypt it with the same key or the process refuses to serve.
const CANARY: &[u8; 18] = b"vivi-store-canary\n";

/// Verifier file created under `DATA_DIR` on the first boot with a key.
pub const VERIFIER_FILE: &str = "store.verifier";

/// AES-256-GCM cipher for stored message content, keyed from
/// `STORE_ENCRYPTION_KEY`.
pub struct StoreCipher {
    cipher: Aes256Gcm,
}

impl StoreCipher {
    /// Parse the env key: standard base64 decoding to exactly 32 bytes.
    pub fn from_base64(raw: &str) -> Result<Self, String> {
        let bytes = STANDARD
            .decode(raw.trim())
            .map_err(|e| format!("STORE_ENCRYPTION_KEY is not valid base64: {e}"))?;
        if bytes.len() != 32 {
            return Err(format!(
                "STORE_ENCRYPTION_KEY must decode to 32 bytes, got {}",
                bytes.len()
            ));
        }
        let cipher = Aes256Gcm::new_from_slice(&bytes)
            .map_err(|_| "STORE_ENCRYPTION_KEY rejected".to_owned())?;
        Ok(Self { cipher })
    }

    /// Encrypt to a self-contained token: base64(12-byte nonce || ciphertext).
    /// Every call uses a fresh random nonce.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<String, String> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = self
            .cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| "encryption failed".to_owned())?;
        let mut token = nonce.to_vec();
        token.extend_from_slice(&ciphertext);
        Ok(STANDARD.encode(token))
    }

    /// Decrypt a token produced by `encrypt`.
    pub fn decrypt(&self, token: &str) -> Result<Vec<u8>, String> {
        let bytes = STANDARD
            .decode(token)
            .map_err(|_| "ciphertext is not valid base64".to_owned())?;
        if bytes.len() < 12 + 16 {
            return Err("ciphertext too short".to_owned());
        }
        let (nonce, ciphertext) = bytes.split_at(12);
        self.cipher
            .decrypt(Nonce::from_slice(nonce), ciphertext)
            .map_err(|_| "decryption failed (wrong key or corrupt record)".to_owned())
    }
}

fn verifier_path(data_dir: &Path) -> PathBuf {
    data_dir.join(VERIFIER_FILE)
}

/// Boot-time key check (U5). A verifier written by an earlier boot must
/// decrypt with the present key; a sealed store with no key, or a wrong key,
/// is a hard failure — the caller must log and exit instead of serving.
/// On the first boot with a key, seals the canary (file created 0600).
pub fn verify_store(data_dir: &Path, cipher: Option<&StoreCipher>) -> Result<(), String> {
    match std::fs::read_to_string(verifier_path(data_dir)) {
        Ok(token) => {
            let cipher = cipher.ok_or_else(|| {
                "STORE_ENCRYPTION_KEY is not set but the store was sealed by an \
                 earlier boot; refusing to serve"
                    .to_owned()
            })?;
            let plain = cipher.decrypt(token.trim()).map_err(|_| {
                "STORE_ENCRYPTION_KEY does not match the key this store was \
                 sealed with; refusing to serve"
                    .to_owned()
            })?;
            if plain != CANARY {
                return Err("store verifier content is corrupt; refusing to serve".to_owned());
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Some(cipher) = cipher {
                seal_verifier(data_dir, cipher)
                    .map_err(|e| format!("failed to write store verifier: {e}"))?;
            }
            Ok(())
        }
        Err(err) => Err(format!("failed to read store verifier: {err}")),
    }
}

/// Seal the canary into `DATA_DIR/store.verifier` (first boot with a key).
fn seal_verifier(data_dir: &Path, cipher: &StoreCipher) -> std::io::Result<()> {
    let token = cipher.encrypt(CANARY).map_err(std::io::Error::other)?;
    std::fs::create_dir_all(data_dir)?;
    let path = verifier_path(data_dir);
    std::fs::write(&path, token.as_bytes())?;
    set_owner_only(&path);
    Ok(())
}

/// Restrict a file to owner-only (0600). No-op on non-unix targets.
pub fn set_owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Walk a room tree enforcing 0700 dirs / 0600 files so sqlite stores,
/// seat identities, sidecars, and journals stay owner-only.
pub fn harden_tree(root: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::symlink_metadata(root) else {
            return;
        };
        if meta.is_dir() {
            let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700));
            let Ok(entries) = std::fs::read_dir(root) else {
                return;
            };
            for entry in entries.flatten() {
                harden_tree(&entry.path());
            }
        } else if meta.is_file() {
            let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = root;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key32(seed: u8) -> String {
        STANDARD.encode([seed; 32])
    }

    #[test]
    fn key_parsing_accepts_exactly_32_bytes() {
        assert!(StoreCipher::from_base64(&key32(1)).is_ok());
        let short = STANDARD.encode([1u8; 31]);
        assert!(StoreCipher::from_base64(&short).is_err());
        assert!(StoreCipher::from_base64("not base64!!").is_err());
    }

    #[test]
    fn round_trip_and_per_record_nonces() {
        let cipher = StoreCipher::from_base64(&key32(7)).unwrap();
        let first = cipher.encrypt(b"hello at rest").unwrap();
        let second = cipher.encrypt(b"hello at rest").unwrap();
        assert_ne!(first, second, "per-record nonces");
        assert_eq!(cipher.decrypt(&first).unwrap(), b"hello at rest");
    }

    #[test]
    fn decrypt_with_wrong_key_fails() {
        let cipher = StoreCipher::from_base64(&key32(1)).unwrap();
        let other = StoreCipher::from_base64(&key32(2)).unwrap();
        let token = cipher.encrypt(b"secret").unwrap();
        assert!(other.decrypt(&token).is_err());
    }

    #[test]
    fn verifier_accepts_right_key_and_rejects_wrong_or_missing() {
        let dir = std::env::temp_dir().join(format!("vivi-server-vc5-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let right = StoreCipher::from_base64(&key32(3)).unwrap();
        let wrong = StoreCipher::from_base64(&key32(4)).unwrap();

        // First boot with a key seals the canary.
        verify_store(&dir, Some(&right)).unwrap();
        assert!(dir.join(VERIFIER_FILE).is_file());
        assert!(verify_store(&dir, Some(&right)).is_ok(), "right key passes");

        let wrong_err = verify_store(&dir, Some(&wrong)).unwrap_err();
        assert!(wrong_err.contains("does not match"), "{wrong_err}");
        let missing_err = verify_store(&dir, None).unwrap_err();
        assert!(missing_err.contains("not set"), "{missing_err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_store_without_key_verifies_clean() {
        let dir =
            std::env::temp_dir().join(format!("vivi-server-vc5-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        verify_store(&dir, None).unwrap();
        assert!(!dir.join(VERIFIER_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
