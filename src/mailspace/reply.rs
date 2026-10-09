use super::delivery::add_header;
use super::{DeliveryResult, Mailspace, SendRequest};
use crate::error::VivariumError;
use crate::message::{ComposeDraft, build_compose_draft};
use crate::storage::{MessageIngestRequest, Storage, StoredMessageView};

pub(super) struct NoteReply {
    pub(super) requests: Vec<MessageIngestRequest>,
    pub(super) data: Vec<u8>,
}

impl Mailspace {
    /// Reply to an existing message. Resolves recipients from the parent
    /// message if none are specified.
    ///
    /// # Errors
    /// Returns an error if the parent handle cannot be resolved, the from
    /// identity is unknown, message composition fails, or delivery fails.
    pub fn reply(
        &self,
        handle: &str,
        from: &str,
        to: Vec<String>,
        cc: Vec<String>,
        subject: Option<String>,
        body: String,
    ) -> Result<DeliveryResult, VivariumError> {
        let storage = self.storage()?;
        let (parent, data) = resolve_reply_parent(&storage, handle)?;
        let parsed = mail_parser::MessageParser::default()
            .parse(&data)
            .ok_or_else(|| VivariumError::Parse("failed to parse reply parent".into()))?;
        let from = self.resolve_identity(from)?;
        let to = if to.is_empty() {
            reply_recipients(self, &parsed, &from)?
        } else {
            to
        };
        let subject =
            subject.unwrap_or_else(|| reply_subject(parsed.subject().unwrap_or("(no subject)")));
        self.send(SendRequest {
            from,
            to,
            cc,
            bcc: Vec::new(),
            subject,
            body,
            role: "inbox".into(),
            kind: Some("mail".into()),
            reply_to: Some(parent.handle),
            depends_on: Vec::new(),
        })
    }

    pub(super) fn note_reply(
        &self,
        storage: &Storage,
        identity: &str,
        parent: &StoredMessageView,
        note: &str,
    ) -> Result<NoteReply, VivariumError> {
        let original = storage.read_message(&parent.message_id)?;
        let parsed = mail_parser::MessageParser::default()
            .parse(&original)
            .ok_or_else(|| VivariumError::Parse("failed to parse lifecycle reply parent".into()))?;
        // Notes on self-addressed items deliver nothing: the actor already
        // knows their own note, and the sent copy (minted below) keeps the
        // thread record. Only other local identities receive an inbox copy.
        let recipients = reply_recipients(self, &parsed, identity).unwrap_or_default();
        let to = if recipients.is_empty() {
            vec![self.address_for(identity)]
        } else {
            self.addresses_for(&recipients)?
        };
        let mut data = build_compose_draft(&ComposeDraft {
            from: self.address_for(identity),
            to,
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: reply_subject(parsed.subject().unwrap_or("(no subject)")),
            body: note.into(),
            html_body: None,
        })?;
        data = add_header(&data, "X-Vivi-Kind", "mail")?;
        data = add_reply_headers(&data, &original)?;
        let mut requests = recipients
            .into_iter()
            .map(|recipient| MessageIngestRequest {
                account: recipient,
                local_role: "inbox".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("lifecycle-note\0{}\0{}", parent.content_id, note),
                remote: None,
            })
            .collect::<Vec<_>>();
        requests.push(MessageIngestRequest {
            account: identity.into(),
            local_role: "sent".into(),
            read_state: true,
            starred: false,
            message_id_hint: None,
            presented_id: None,
            seed_hint: format!("lifecycle-note-sent\0{}\0{}", parent.content_id, note),
            remote: None,
        });
        Ok(NoteReply {
            data: data.into_bytes(),
            requests,
        })
    }
}

pub(super) fn resolve_reply_parent(
    storage: &Storage,
    handle: &str,
) -> Result<(StoredMessageView, Vec<u8>), VivariumError> {
    let message_id = storage.resolve_message_token(handle)?;
    let parent = storage
        .message_by_id(&message_id)?
        .ok_or_else(|| VivariumError::Message(format!("message not found: {handle}")))?;
    let data = storage.read_message(&message_id)?;
    Ok((parent, data))
}

pub(super) fn add_reply_headers(eml: &str, parent: &[u8]) -> Result<String, VivariumError> {
    let parsed = mail_parser::MessageParser::default()
        .parse(parent)
        .ok_or_else(|| VivariumError::Parse("failed to parse reply parent".into()))?;
    let message_id = parsed
        .message_id()
        .ok_or_else(|| VivariumError::Message("reply parent has no Message-ID".into()))?;
    let eml = add_header(eml, "In-Reply-To", message_id)?;
    add_header(&eml, "References", message_id)
}

fn reply_recipients(
    mailspace: &Mailspace,
    parsed: &mail_parser::Message<'_>,
    from: &str,
) -> Result<Vec<String>, VivariumError> {
    let mut recipients = local_recipients(mailspace, parsed.from(), from);
    if recipients.is_empty()
        && let Some(addresses) = parsed.to()
    {
        recipients = local_recipients(mailspace, Some(addresses), from);
    }
    if recipients.is_empty() {
        return Err(VivariumError::Message(
            "reply parent has no other local identity to receive the reply".into(),
        ));
    }
    Ok(recipients)
}

fn local_recipients(
    mailspace: &Mailspace,
    addresses: Option<&mail_parser::Address<'_>>,
    from: &str,
) -> Vec<String> {
    let mut recipients = Vec::new();
    let Some(addresses) = addresses else {
        return recipients;
    };
    for address in addresses.iter() {
        let Some(value) = address.address.as_deref() else {
            continue;
        };
        let Ok(identity) = mailspace.resolve_identity(value) else {
            continue;
        };
        if identity != from && !recipients.contains(&identity) {
            recipients.push(identity);
        }
    }
    recipients
}

fn reply_subject(subject: &str) -> String {
    if has_reply_prefix(subject) {
        subject.to_string()
    } else {
        format!("Re: {subject}")
    }
}

fn has_reply_prefix(subject: &str) -> bool {
    let lower = subject.trim().to_ascii_lowercase();
    let Some(after_re) = lower.strip_prefix("re") else {
        return false;
    };
    if after_re.starts_with(':') {
        return true;
    }
    let Some(closing) = after_re.strip_prefix('[').and_then(|value| value.find(']')) else {
        return false;
    };
    after_re
        .get(closing + 1..)
        .is_some_and(|value| value.starts_with(':'))
}
