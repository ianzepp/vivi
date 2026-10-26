//! ED25519 challenge/verify auth per docs/contract.md (hardening summary is
//! authoritative): SPKI PEM in, canonical SPKI DER fingerprint stored,
//! single-use 120 s challenges, server-minted bearer tokens kept only as
//! SHA-256 digests with constant-time comparison.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;
use ed25519_dalek::pkcs8::{DecodePublicKey, EncodePublicKey};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const CHALLENGE_TTL_SECS: u64 = 120;
pub const CHALLENGE_BYTES: usize = 32;
pub const TOKEN_TTL_HOURS: i64 = 1;
pub const TOKEN_BYTES: usize = 32;

/// Crypto / auth primitive failures inside this module.
#[derive(Debug)]
pub enum AuthError {
    InvalidKey,
    Entropy,
    ChallengeFailed,
}

// ---------------------------------------------------------------- crypto

/// Normalize a PEM string: keep base64 body lines, drop armor, trim space.
fn pem_body(public_key_pem: &str) -> Result<String, AuthError> {
    if !public_key_pem.contains("-----BEGIN PUBLIC KEY-----")
        || !public_key_pem.contains("-----END PUBLIC KEY-----")
    {
        return Err(AuthError::InvalidKey);
    }
    let body: String = public_key_pem
        .lines()
        .filter(|line| !line.contains("-----"))
        .map(str::trim)
        .collect();
    Ok(body)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    hex_encode(&digest)
}

/// Parse an SPKI PEM into a verifying key.
pub fn parse_verifying_key(public_key_pem: &str) -> Result<VerifyingKey, AuthError> {
    let body = pem_body(public_key_pem)?;
    let der = STANDARD
        .decode(body.as_bytes())
        .map_err(|_| AuthError::InvalidKey)?;
    VerifyingKey::from_public_key_der(&der).map_err(|_| AuthError::InvalidKey)
}

/// Rebuild a key from the canonical SPKI DER hex persisted per seat.
pub fn verifying_key_from_spki_hex(spki_hex: &str) -> Result<VerifyingKey, AuthError> {
    let der = hex_decode(spki_hex).ok_or(AuthError::InvalidKey)?;
    VerifyingKey::from_public_key_der(&der).map_err(|_| AuthError::InvalidKey)
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// Canonical SPKI DER hex (persisted identity material) for a key.
pub fn canonical_spki_hex(key: &VerifyingKey) -> Result<String, AuthError> {
    let der = key.to_public_key_der().map_err(|_| AuthError::InvalidKey)?;
    Ok(hex_encode(der.as_bytes()))
}

/// Fingerprint: lowercase hex SHA-256 over canonical SPKI DER (64 chars).
pub fn key_fingerprint(key: &VerifyingKey) -> Result<String, AuthError> {
    Ok(sha256_hex(canonical_spki_hex(key)?.as_bytes()))
}

/// Verify an ED25519 signature over the exact challenge bytes as issued.
pub fn verify_signature(key: &VerifyingKey, message: &[u8], signature_base64: &str) -> bool {
    let Ok(sig_bytes) = STANDARD.decode(signature_base64.trim().as_bytes()) else {
        return false;
    };
    let Ok(signature) = Signature::from_slice(&sig_bytes) else {
        return false;
    };
    key.verify(message, &signature).is_ok()
}

/// Constant-time compare for token digests.
pub fn constant_time_eq(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

pub fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// A freshly minted bearer token: raw form (client-only) plus at-rest record.
pub struct MintedToken {
    pub token: String,
    pub digest: String,
    pub expires_at: DateTime<Utc>,
}

pub fn mint_token() -> Result<MintedToken, AuthError> {
    mint_token_until(Utc::now() + chrono::Duration::hours(TOKEN_TTL_HOURS))
}

/// Mint a bearer that lives until `expires_at` (chat guests last as long as
/// the room; key-bound members keep the one-hour TTL).
pub fn mint_token_until(expires_at: DateTime<Utc>) -> Result<MintedToken, AuthError> {
    let mut raw = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut raw).map_err(|_| AuthError::Entropy)?;
    let token = URL_SAFE_NO_PAD.encode(raw);
    let digest = sha256_hex(token.as_bytes());
    Ok(MintedToken {
        token,
        digest,
        expires_at,
    })
}

// ------------------------------------------------------------- challenges

#[derive(Debug)]
struct ChallengeEntry {
    room_id: String,
    fingerprint: String,
    challenge: [u8; CHALLENGE_BYTES],
    expires_at: DateTime<Utc>,
}

pub struct IssuedChallenge {
    pub challenge_id: String,
    pub challenge: String,
    pub expires_in: u64,
}

/// Single-use, in-memory challenge book keyed by opaque challenge id.
/// Challenges are consumed on first verification attempt (success or not).
#[derive(Clone, Default)]
pub struct ChallengeBook(Arc<Mutex<HashMap<String, ChallengeEntry>>>);

impl ChallengeBook {
    fn guard(&self) -> MutexGuard<'_, HashMap<String, ChallengeEntry>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn issue(&self, room_id: &str, fingerprint: &str) -> Result<IssuedChallenge, AuthError> {
        let mut bytes = [0u8; CHALLENGE_BYTES];
        let mut id = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| AuthError::Entropy)?;
        getrandom::fill(&mut id).map_err(|_| AuthError::Entropy)?;
        let challenge_id = URL_SAFE_NO_PAD.encode(id);
        let entry = ChallengeEntry {
            room_id: room_id.to_owned(),
            fingerprint: fingerprint.to_owned(),
            challenge: bytes,
            expires_at: Utc::now() + chrono::Duration::seconds(CHALLENGE_TTL_SECS as i64),
        };
        self.guard().insert(challenge_id.clone(), entry);
        Ok(IssuedChallenge {
            challenge_id,
            challenge: STANDARD.encode(bytes),
            expires_in: CHALLENGE_TTL_SECS,
        })
    }

    /// Fingerprint a challenge was issued for, without consuming it.
    pub fn peek(&self, challenge_id: &str) -> Option<(String, String)> {
        let guard = self.guard();
        let entry = guard.get(challenge_id)?;
        Some((entry.room_id.clone(), entry.fingerprint.clone()))
    }

    /// Consume a challenge for `(room_id, fingerprint)`; removal happens
    /// before any check so a challenge is spent on its first use.
    pub fn consume(
        &self,
        room_id: &str,
        challenge_id: &str,
        fingerprint: &str,
    ) -> Result<[u8; CHALLENGE_BYTES], AuthError> {
        let mut guard = self.guard();
        let entry = guard
            .remove(challenge_id)
            .ok_or(AuthError::ChallengeFailed)?;
        if entry.room_id != room_id
            || entry.fingerprint != fingerprint
            || Utc::now() >= entry.expires_at
        {
            return Err(AuthError::ChallengeFailed);
        }
        Ok(entry.challenge)
    }
}
