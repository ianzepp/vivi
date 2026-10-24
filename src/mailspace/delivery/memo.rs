//! Self-addressed memos: save and search.

use chrono::Utc;

use super::super::Mailspace;
use super::add_header;
use super::retention::prune_retained_memos;
use crate::error::VivariumError;
use crate::letter::gate as letter_gate;
use crate::letter::plain::{MemoFront, render};
use crate::letter::{FORMAT_HEADER, FORMAT_VERSION};
use crate::message::{ComposeDraft, build_compose_draft};
use crate::storage::{MessageIngestRequest, StoredMessageView};

impl Mailspace {
    /// Save a self-addressed memo into the identity's `memos` folder.
    /// Unlike `send`, this creates no sent copy and no recipient delivery —
    /// it is a single-actor persistence operation. The memo is a letter: its
    /// tags ride the `MemoFront` front matter (bare prose when there are
    /// none) and the stored bytes carry `X-Vivi-Format: 1`.
    ///
    /// # Errors
    /// Returns an error if the identity cannot be resolved, a tag is empty or
    /// contains whitespace, message composition fails, or the storage
    /// operation fails.
    pub fn save_memo(
        &self,
        identity: &str,
        subject: &str,
        body: &str,
        tags: &[String],
    ) -> Result<String, VivariumError> {
        let tags = normalized_memo_tags(tags)?;
        let identity = self.resolve_identity(identity)?;
        let address = self.address_for(&identity);
        let body = render(&MemoFront { tags: tags.clone() }, body)
            .map_err(|err| VivariumError::Message(format!("memo letter: {err}")))?;
        let gated = letter_gate::gate("memo", &body)
            .map_err(|err| VivariumError::Message(err.to_string()))?;
        let mut eml = build_compose_draft(&ComposeDraft {
            from: address.clone(),
            to: vec![address],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.to_string(),
            body,
            html_body: None,
        })?;
        eml = add_header(&eml, "X-Vivi-Kind", &gated.kind)?;
        if gated.formatted {
            eml = add_header(&eml, FORMAT_HEADER, FORMAT_VERSION)?;
        }
        let seed = Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let storage = self.storage()?;
        let stored = storage.ingest_message(
            &MessageIngestRequest {
                account: identity.clone(),
                local_role: "memos".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("memo-save\0{seed}\0{identity}"),
                remote: None,
            },
            eml.as_bytes(),
        )?;
        let handle = storage.display_handle(&stored.message_id)?;
        drop(storage);
        prune_retained_memos(self, &identity, &tags)?;
        Ok(handle)
    }

    /// Search memos for an identity by query.
    ///
    /// Matches subject and body (unless `subject_only` is true). Search is
    /// case-insensitive. Only memos whose subject does not already match are
    /// parsed for body text, so the common case is cheap.
    ///
    /// # Errors
    /// Returns an error if the identity cannot be resolved, a storage
    /// operation fails, or a memo blob cannot be parsed.
    pub fn search_memos(
        &self,
        identity: &str,
        query: &str,
        subject_only: bool,
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        let query = query.to_lowercase();
        let memos = self.list_kind(Some(identity), "memos", "memo")?;
        if subject_only {
            return Ok(memos
                .into_iter()
                .filter(|memo| memo.subject.to_lowercase().contains(&query))
                .collect());
        }
        let storage = self.storage()?;
        let mut matched = Vec::new();
        for memo in memos {
            if memo.subject.to_lowercase().contains(&query) {
                matched.push(memo);
                continue;
            }
            let data = storage.read_message(&memo.message_id)?;
            let extracted = crate::extract::extract_text(&data)?;
            if extracted.to_lowercase().contains(&query) {
                matched.push(memo);
            }
        }
        Ok(matched)
    }
}

/// Trim memo tags and reject ones that are empty or contain whitespace, so
/// every stored tag is a single token.
fn normalized_memo_tags(tags: &[String]) -> Result<Vec<String>, VivariumError> {
    let mut normalized = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = tag.trim();
        if tag.is_empty() || tag.chars().any(char::is_whitespace) {
            return Err(VivariumError::Message(format!(
                "memo tag {tag:?} must be non-empty and contain no whitespace"
            )));
        }
        normalized.push(tag.to_string());
    }
    Ok(normalized)
}
