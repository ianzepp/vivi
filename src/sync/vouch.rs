//! Governance vouch letters: one admitted member vouches for another's key.
//!
//! A vouch is one governance letter appended to the project with this
//! mailspace's own bearer. It is never stored or filed locally; it only goes
//! to the server, whose admission rules act on it.

use rand_core::{OsRng, RngCore};
use serde_json::json;

use crate::error::VivariumError;
use crate::sync::transport::{AppendFailure, Appended, Envelope, Transport};

/// The envelope recipient of every governance letter.
const POSTMASTER: &str = "postmaster";

/// The marker a member key must carry to be a public-key PEM.
const PEM_BEGIN: &str = "-----BEGIN PUBLIC KEY-----";

/// Bytes of randomness in a vouch's wire id and Message-ID (24 hex chars).
const RANDOM_BYTES: usize = 12;

/// A composed vouch letter and the wire id it travels under.
#[derive(Debug, Clone)]
pub struct VouchLetter {
    /// The client-minted wire id: `msg_` plus 24 lowercase hex characters.
    pub message_id: String,
    /// The exact letter bytes to append.
    pub raw_eml: Vec<u8>,
}

/// Compose one vouch letter: governance mail from this mailspace's own
/// address to the project's postmaster, its JSON body naming `member` and
/// `public_key_pem`.
///
/// Refuses before anything is sent: `member` must be a bare name with no
/// whitespace, and `public_key_pem` must be a public-key PEM.
///
/// # Errors
/// Returns an error for an empty or whitespace member name, or a PEM
/// without the public-key marker.
pub fn compose_vouch(
    sender_name: &str,
    address_domain: &str,
    member: &str,
    public_key_pem: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<VouchLetter, VivariumError> {
    if member.is_empty() || member.chars().any(char::is_whitespace) {
        return Err(VivariumError::Message(format!(
            "invalid member name '{member}': a vouch names one member, with no whitespace"
        )));
    }
    if !public_key_pem.contains(PEM_BEGIN) {
        return Err(VivariumError::Message(
            "the member key is not a public key PEM (no '-----BEGIN PUBLIC KEY-----')".into(),
        ));
    }
    let basis = random_hex();
    let from = format!("{sender_name}@{address_domain}");
    let raw_eml = format!(
        "From: {from}\n\
         To: {POSTMASTER}@{address_domain}\n\
         Subject: vouch {member}\n\
         Message-ID: <vouch-{member}-{basis}@{address_domain}>\n\
         Date: {}\n\
         MIME-Version: 1.0\n\
         Content-Type: text/plain; charset=utf-8\n\
         \n\
         {}\n",
        now.to_rfc2822(),
        json!({"vouch": {"name": member, "public_key_pem": public_key_pem}})
    )
    .into_bytes();
    Ok(VouchLetter {
        message_id: format!("msg_{basis}"),
        raw_eml,
    })
}

/// Append the letter to the project with the transport's bearer.
///
/// # Errors
/// Maps every append failure (refusal, stop, transport error) into an error
/// carrying the server's reason.
pub fn send_vouch(
    transport: &dyn Transport,
    letter: &VouchLetter,
) -> Result<Appended, VivariumError> {
    let envelope = Envelope {
        raw_eml: &letter.raw_eml,
        to: POSTMASTER,
        kind: "mail",
    };
    transport
        .append(&letter.message_id, &envelope)
        .map_err(|failure| {
            VivariumError::Message(match failure {
                AppendFailure::Refused(reason) | AppendFailure::Stop(reason) => reason,
            })
        })
}

/// Random lowercase hex for the wire id and the Message-ID.
fn random_hex() -> String {
    let mut bytes = [0u8; RANDOM_BYTES];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}
