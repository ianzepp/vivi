//! Governance vouch letters: one admitted member vouches for another's key.
//!
//! A vouch is one governance letter appended to the project with this
//! mailspace's own bearer. It is never stored or filed locally; it only goes
//! to the server, whose admission rules act on it. The letter is format-1
//! mail. Its prose is the vouch JSON the server admits on; that object is
//! not a mail front, so the body has no fence.

use rand_core::{OsRng, RngCore};
use serde_json::json;

use crate::error::VivariumError;
use crate::letter::{self, FORMAT_HEADER, FORMAT_VERSION};
use crate::sync::transport::{AppendFailure, Appended, Envelope, Transport};

/// The envelope recipient of every governance letter.
const POSTMASTER: &str = "postmaster";

/// The marker a member key must carry to be a public-key PEM.
const PEM_BEGIN: &str = "-----BEGIN PUBLIC KEY-----";

/// The kind a vouch travels as. It is governance mail, not a work letter.
const KIND: &str = "mail";

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

/// Compose one vouch letter: format-1 mail from this mailspace's own
/// address to the project's postmaster. The prose is the JSON object
/// naming `member` and `public_key_pem`, which is the fact the server
/// admits on. The body goes through the mail gate, and the composed bytes
/// are parsed back, so a writer bug is an error before anything is sent.
///
/// Refuses before anything is sent: `member` must be a bare name with no
/// whitespace, and `public_key_pem` must be a public-key PEM.
///
/// # Errors
/// Returns an error for an empty or whitespace member name, a PEM without
/// the public-key marker, a body the mail gate refuses, or composed bytes
/// that do not parse as the format-1 mail letter they are.
pub fn compose_vouch(
    sender_name: &str,
    address_domain: &str,
    member: &str,
    public_key_pem: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<VouchLetter, VivariumError> {
    check_member(member)?;
    check_pem(public_key_pem)?;
    let basis = random_hex();
    let body = vouch_prose(member, public_key_pem)?;
    let raw_eml = render_letter(sender_name, address_domain, member, &basis, &body, now);
    letter::of_message(&raw_eml, &format!("vouch-{member}"))
        .map_err(|err| VivariumError::Message(format!("vouch letter: {err}")))?;
    Ok(VouchLetter {
        message_id: format!("msg_{basis}"),
        raw_eml,
    })
}

fn check_member(member: &str) -> Result<(), VivariumError> {
    if member.is_empty() || member.chars().any(char::is_whitespace) {
        return Err(VivariumError::Message(format!(
            "invalid member name '{member}': a vouch names one member, with no whitespace"
        )));
    }
    Ok(())
}

fn check_pem(public_key_pem: &str) -> Result<(), VivariumError> {
    if public_key_pem.contains(PEM_BEGIN) {
        return Ok(());
    }
    Err(VivariumError::Message(
        "the member key is not a public key PEM (no '-----BEGIN PUBLIC KEY-----')".into(),
    ))
}

/// The JSON prose, accepted by the mail gate as a format-1 body.
fn vouch_prose(member: &str, public_key_pem: &str) -> Result<String, VivariumError> {
    let body = json!({"vouch": {"name": member, "public_key_pem": public_key_pem}}).to_string();
    let gated = letter::gate(KIND, &body)
        .map_err(|err| VivariumError::Message(format!("vouch letter: {err}")))?;
    if gated.formatted && gated.kind == KIND {
        return Ok(body);
    }
    Err(VivariumError::Message(
        "vouch letter: the mail gate did not accept the vouch body as format-1 mail".into(),
    ))
}

fn render_letter(
    sender_name: &str,
    address_domain: &str,
    member: &str,
    basis: &str,
    body: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<u8> {
    let from = format!("{sender_name}@{address_domain}");
    format!(
        "From: {from}\n\
         To: {POSTMASTER}@{address_domain}\n\
         Subject: vouch {member}\n\
         Message-ID: <vouch-{member}-{basis}@{address_domain}>\n\
         Date: {}\n\
         MIME-Version: 1.0\n\
         Content-Type: text/plain; charset=utf-8\n\
         X-Vivi-Kind: {KIND}\n\
         {FORMAT_HEADER}: {FORMAT_VERSION}\n\
         \n\
         {body}\n",
        now.to_rfc2822(),
    )
    .into_bytes()
}

/// Append the letter to the project with the transport's bearer.
///
/// # Errors
/// Maps every append failure (refusal, rate limit, stop, transport error)
/// into an error carrying the server's reason.
pub fn send_vouch(
    transport: &dyn Transport,
    letter: &VouchLetter,
) -> Result<Appended, VivariumError> {
    let envelope = Envelope {
        raw_eml: &letter.raw_eml,
        to: POSTMASTER,
        kind: KIND,
    };
    transport
        .append(&letter.message_id, &envelope)
        .map_err(|failure| {
            VivariumError::Message(match failure {
                AppendFailure::Refused(reason)
                | AppendFailure::RateLimited(reason)
                | AppendFailure::Stop(reason) => reason,
            })
        })
}

/// Random lowercase hex for the wire id and the Message-ID.
fn random_hex() -> String {
    let mut bytes = [0u8; RANDOM_BYTES];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}
