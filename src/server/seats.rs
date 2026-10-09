//! Per-room seat registry: bindings persisted at
//! `DATA_DIR/rooms/<room_id>/seats.json` (0600), one record per seat with the
//! canonical SPKI DER hex, unique label, and bearer-token records stored only
//! as SHA-256 digests plus expiry. Raw tokens never touch disk.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::auth::{
    AuthError, ChallengeBook, canonical_spki_hex, constant_time_eq, key_fingerprint, mint_token,
    parse_verifying_key, rfc3339, sha256_hex, verify_signature, verifying_key_from_spki_hex,
};
use crate::rooms::RoomError;

pub const MAX_LABEL_LEN: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRecord {
    pub digest: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeatRecord {
    pub seat: u32,
    pub label: String,
    pub fingerprint: String,
    /// Canonical SPKI DER, hex. Public key material; needed to verify
    /// re-signed refresh challenges. Raw tokens are never persisted.
    pub spki_der: String,
    pub tokens: Vec<TokenRecord>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SeatsFile {
    pub seats: Vec<SeatRecord>,
}

#[derive(Debug)]
pub enum JoinError {
    Room(RoomError),
    InvalidKey,
    InvalidLabel,
    ChallengeFailed,
    SeatFull,
    LabelTaken,
    Storage(String),
}

/// Seat binding outcome returned to the client once.
#[derive(Debug, Clone)]
pub struct JoinOutcome {
    pub seat: u32,
    pub label: String,
    pub token: String,
    pub expires_at: String,
    pub fingerprint: String,
}

#[derive(Clone)]
pub struct SeatRegistry {
    lock: Arc<Mutex<()>>,
    root: PathBuf,
}

fn token_record(digest: String, expires_at: DateTime<Utc>) -> TokenRecord {
    TokenRecord {
        digest,
        expires_at: rfc3339(expires_at),
    }
}

fn not_expired(record: &TokenRecord) -> bool {
    match DateTime::parse_from_rfc3339(&record.expires_at) {
        Ok(expires_at) => expires_at.with_timezone(&Utc) > Utc::now(),
        Err(_) => false,
    }
}

impl From<RoomError> for JoinError {
    fn from(value: RoomError) -> Self {
        JoinError::Room(value)
    }
}

fn validate_label(
    label: &str,
    seats: &[SeatRecord],
    except_seat: Option<u32>,
) -> Result<String, JoinError> {
    let trimmed = label.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_LABEL_LEN {
        return Err(JoinError::InvalidLabel);
    }
    // Labels double as vivi identity local-parts when the room mailspace
    // provisions per-seat senders (contract R4): keep to that charset.
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(JoinError::InvalidLabel);
    }
    let taken = seats
        .iter()
        .any(|s| Some(s.seat) != except_seat && s.label == trimmed);
    if taken {
        return Err(JoinError::LabelTaken);
    }
    Ok(trimmed.to_owned())
}

impl SeatRegistry {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            lock: Arc::new(Mutex::new(())),
            root: data_dir.join("rooms"),
        }
    }

    fn seats_path(&self, room_id: &str) -> PathBuf {
        self.root.join(room_id).join("seats.json")
    }

    fn load(&self, room_id: &str) -> SeatsFile {
        std::fs::read(self.seats_path(room_id))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Atomic 0600 persistence via temp file + rename.
    fn save(&self, room_id: &str, file: &SeatsFile) -> Result<(), String> {
        let mut pruned = SeatsFile {
            seats: file.seats.clone(),
        };
        for seat in &mut pruned.seats {
            seat.tokens.retain(not_expired);
        }
        let bytes =
            serde_json::to_vec(&pruned).map_err(|e| format!("failed to encode seats.json: {e}"))?;
        let path = self.seats_path(room_id);
        let tmp = path.with_extension("json.tmp");
        write_private(&tmp, &bytes).map_err(|e| format!("failed to write seats.json: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("failed to place seats.json: {e}"))
    }

    fn mint_record(&self) -> Result<(String, TokenRecord), JoinError> {
        let minted = mint_token().map_err(|err| match err {
            AuthError::Entropy => JoinError::Storage("entropy unavailable".into()),
            _ => JoinError::ChallengeFailed,
        })?;
        let record = token_record(minted.digest.clone(), minted.expires_at);
        Ok((minted.token, record))
    }

    /// Is `fingerprint` bound to any seat in the room? (refresh gate)
    pub fn fingerprint_bound(&self, room_id: &str, fingerprint: &str) -> bool {
        let _guard = self.lock();
        self.load(room_id)
            .seats
            .iter()
            .any(|s| s.fingerprint == fingerprint)
    }

    /// Bind the room creator to seat 1; no challenge step (contract: the
    /// creator auto-binds seat 1 at create time).
    pub fn bind_creator(
        &self,
        room_id: &str,
        public_key_pem: &str,
        label: Option<String>,
    ) -> Result<JoinOutcome, JoinError> {
        let _guard = self.lock();
        let key = parse_verifying_key(public_key_pem).map_err(|_| JoinError::InvalidKey)?;
        let fingerprint = key_fingerprint(&key).map_err(|_| JoinError::InvalidKey)?;
        let spki_der = canonical_spki_hex(&key).map_err(|_| JoinError::InvalidKey)?;
        let mut file = self.load(room_id);
        if file.seats.iter().any(|s| s.seat == 1) {
            return Err(JoinError::Storage("seat 1 already bound".into()));
        }
        let label = match label {
            Some(value) => validate_label(&value, &file.seats, None)?,
            None => "seat-1".to_owned(),
        };
        let (token, record) = self.mint_record()?;
        let expires_at = record.expires_at.clone();
        file.seats.push(SeatRecord {
            seat: 1,
            label: label.clone(),
            fingerprint: fingerprint.clone(),
            spki_der,
            tokens: vec![record],
        });
        self.save(room_id, &file).map_err(JoinError::Storage)?;
        Ok(JoinOutcome {
            seat: 1,
            label,
            token,
            expires_at,
            fingerprint,
        })
    }

    /// Verify a join: consume the challenge (single-use), verify the
    /// signature under the presented key, then bind the next open seat
    /// (201) or rebind the held seat (200).
    pub fn join(
        &self,
        room_id: &str,
        capacity: u32,
        req: &crate::auth::JoinRequest,
        book: &ChallengeBook,
    ) -> Result<(bool, JoinOutcome), JoinError> {
        let _guard = self.lock();
        let key = parse_verifying_key(&req.public_key_pem).map_err(|_| JoinError::InvalidKey)?;
        let fingerprint = key_fingerprint(&key).map_err(|_| JoinError::InvalidKey)?;
        let challenge = book
            .consume(room_id, &req.challenge_id, &fingerprint)
            .map_err(|_| JoinError::ChallengeFailed)?;
        if !verify_signature(&key, &challenge, &req.signature_base64) {
            return Err(JoinError::ChallengeFailed);
        }
        let mut file = self.load(room_id);
        if let Some(index) = file.seats.iter().position(|s| s.fingerprint == fingerprint) {
            let seat = file.seats[index].seat;
            let label = match &req.label {
                Some(value) => validate_label(value, &file.seats, Some(seat))?,
                None => file.seats[index].label.clone(),
            };
            let (token, record) = self.mint_record()?;
            let binding = &mut file.seats[index];
            binding.label = label.clone();
            binding.tokens.push(record.clone());
            self.save(room_id, &file).map_err(JoinError::Storage)?;
            return Ok((
                false,
                JoinOutcome {
                    seat,
                    label,
                    token,
                    expires_at: record.expires_at,
                    fingerprint,
                },
            ));
        }
        let seat = open_seat(&file, capacity).ok_or(JoinError::SeatFull)?;
        let label = match &req.label {
            Some(value) => validate_label(value, &file.seats, None)?,
            None => format!("seat-{seat}"),
        };
        let (token, record) = self.mint_record()?;
        file.seats.push(SeatRecord {
            seat,
            label: label.clone(),
            fingerprint: fingerprint.clone(),
            spki_der: canonical_spki_hex(&key).map_err(|_| JoinError::InvalidKey)?,
            tokens: vec![record.clone()],
        });
        self.save(room_id, &file).map_err(JoinError::Storage)?;
        Ok((
            true,
            JoinOutcome {
                seat,
                label,
                token,
                expires_at: record.expires_at,
                fingerprint,
            },
        ))
    }

    /// Refresh a bearer for a held seat. The challenge must target the bound
    /// fingerprint and re-sign under the stored key; failures fail closed.
    pub fn refresh_token(
        &self,
        room_id: &str,
        req: &crate::auth::TokenRefreshRequest,
        book: &ChallengeBook,
    ) -> Result<(String, String), JoinError> {
        let _guard = self.lock();
        let (_, fingerprint) = book
            .peek(&req.challenge_id)
            .filter(|(room, _)| room == room_id)
            .ok_or(JoinError::ChallengeFailed)?;
        let mut file = self.load(room_id);
        let index = file
            .seats
            .iter()
            .position(|s| s.fingerprint == fingerprint)
            .ok_or(JoinError::ChallengeFailed)?;
        let key = verifying_key_from_spki_hex(&file.seats[index].spki_der)
            .map_err(|_| JoinError::ChallengeFailed)?;
        let challenge = book
            .consume(room_id, &req.challenge_id, &fingerprint)
            .map_err(|_| JoinError::ChallengeFailed)?;
        if !verify_signature(&key, &challenge, &req.signature_base64) {
            return Err(JoinError::ChallengeFailed);
        }
        let (token, record) = self.mint_record()?;
        file.seats[index].tokens.push(record.clone());
        self.save(room_id, &file).map_err(JoinError::Storage)?;
        Ok((token, record.expires_at))
    }

    /// Resolve a presented bearer to its seat. Digest comparison is
    /// constant-time; expired records never match.
    pub fn verify_bearer(&self, room_id: &str, presented: &str) -> Option<u32> {
        let _guard = self.lock();
        let digest = sha256_hex(presented.as_bytes());
        let file = self.load(room_id);
        for seat in &file.seats {
            for record in &seat.tokens {
                if not_expired(record) && constant_time_eq(&digest, &record.digest) {
                    return Some(seat.seat);
                }
            }
        }
        None
    }

    /// The full seat record a presented bearer resolves to (VC-4 origin
    /// verification needs the label and fingerprint, not just the number).
    pub fn bearer_seat(&self, room_id: &str, presented: &str) -> Option<SeatRecord> {
        let _guard = self.lock();
        let digest = sha256_hex(presented.as_bytes());
        self.load(room_id).seats.into_iter().find(|seat| {
            seat.tokens
                .iter()
                .any(|record| not_expired(record) && constant_time_eq(&digest, &record.digest))
        })
    }

    /// Whether `label` is a bound seat label (direct-send validation).
    pub fn has_label(&self, room_id: &str, label: &str) -> bool {
        let _guard = self.lock();
        self.load(room_id).seats.iter().any(|s| s.label == label)
    }

    /// Labels of every bound seat except the given seat (broadcast targets).
    pub fn labels_except(&self, room_id: &str, seat: u32) -> Vec<String> {
        let _guard = self.lock();
        self.load(room_id)
            .seats
            .iter()
            .filter(|s| s.seat != seat)
            .map(|s| s.label.clone())
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn open_seat(file: &SeatsFile, capacity: u32) -> Option<u32> {
    (1..=capacity).find(|n| !file.seats.iter().any(|s| s.seat == *n))
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
