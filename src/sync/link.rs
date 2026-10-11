//! Reading a letter's headers: reply linking, addresses.
//!
//! A letter is the only carrier of its reply relation: the envelope has no
//! parent field. The parent is whichever RFC `Message-ID` the letter's
//! `In-Reply-To` (else the last `References` id) names; it is looked up among
//! the live rows' `Message-ID` headers, never by wire id.

use crate::error::VivariumError;
use crate::message::normalize_message_id;
use crate::storage::Storage;

/// The headers of a pulled letter that sync reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LetterHeaders {
    pub subject: String,
    /// Normalized RFC `Message-ID` of the letter this one answers.
    pub parent: Option<String>,
    /// The first From address, lowercased.
    pub from: Option<String>,
    /// The letter's own To then Cc addresses, bare and lowercased,
    /// deduplicated, in header order.
    pub recipients: Vec<String>,
}

impl LetterHeaders {
    /// The recipients as the envelope declares them: sorted and joined with
    /// `,`. Empty when the letter names none.
    #[must_use]
    pub fn envelope_to(&self) -> String {
        let mut sorted = self.recipients.clone();
        sorted.sort();
        sorted.join(",")
    }
}

/// What [`link_reply`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOutcome {
    /// A new reply link was recorded.
    Linked,
    /// The child already had a parent link, or is its own parent's content.
    Settled,
    /// No live row carries the parent's `Message-ID` yet.
    ParentMissing,
}

/// Read the subject and reply parent out of a letter's bytes.
#[must_use]
pub fn parse_headers(data: &[u8]) -> LetterHeaders {
    let Some(parsed) = mail_parser::MessageParser::default().parse(data) else {
        return LetterHeaders::default();
    };
    let cited = parsed
        .in_reply_to()
        .as_text()
        .or_else(|| parsed.references().as_text());
    let mut recipients: Vec<String> = Vec::new();
    for list in [parsed.to(), parsed.cc()].into_iter().flatten() {
        for address in list.iter().filter_map(|addr| addr.address.as_deref()) {
            let address = address.trim().to_ascii_lowercase();
            if !address.is_empty() && !recipients.contains(&address) {
                recipients.push(address);
            }
        }
    }
    let from = parsed
        .from()
        .and_then(|list| list.first())
        .and_then(|addr| addr.address.as_deref())
        .map(|address| address.trim().to_ascii_lowercase())
        .filter(|address| !address.is_empty());
    LetterHeaders {
        subject: parsed.subject().unwrap_or_default().to_string(),
        parent: cited.and_then(normalize_message_id),
        from,
        recipients,
    }
}

/// Record that the content `child_content_id` answers the live row carrying
/// RFC id `parent_rfc_id`, unless the child already has a parent link.
///
/// # Errors
/// Returns an error on a storage failure.
pub fn link_reply(
    storage: &Storage,
    child_content_id: &str,
    parent_rfc_id: &str,
) -> Result<LinkOutcome, VivariumError> {
    if storage
        .mailspace_link_for_child(child_content_id)?
        .is_some()
    {
        return Ok(LinkOutcome::Settled);
    }
    let Some(parent) = storage.live_message_by_rfc_id(parent_rfc_id)? else {
        return Ok(LinkOutcome::ParentMissing);
    };
    if parent.content_id == child_content_id {
        return Ok(LinkOutcome::Settled);
    }
    storage.link_mailspace_content(child_content_id, &parent.content_id, "captured")?;
    Ok(LinkOutcome::Linked)
}
