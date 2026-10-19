use std::collections::BTreeSet;

use crate::error::VivariumError;
use crate::mailspace::DeliveredMessage;
use crate::storage::{MessageIngestRequest, Storage, StoredMessage};

pub(in crate::mailspace) struct DeliveredMessageId {
    pub(in crate::mailspace) identity: String,
    pub(in crate::mailspace) message_id: String,
    pub(in crate::mailspace) content_id: String,
}

/// One local-only request per resolved recipient for a raw delivery.
pub(super) fn raw_delivery_requests(
    recipients: &[String],
    role: &str,
    data_len: usize,
) -> Vec<MessageIngestRequest> {
    recipients
        .iter()
        .map(|recipient| MessageIngestRequest {
            account: recipient.clone(),
            local_role: role.to_string(),
            read_state: false,
            starred: false,
            message_id_hint: None,
            presented_id: None,
            seed_hint: format!("raw-delivery\0{recipient}\0{data_len}"),
            remote: None,
        })
        .collect()
}

pub(super) fn ingest_for_recipients(
    storage: &Storage,
    recipients: BTreeSet<String>,
    role: &str,
    eml: &str,
    seed: i64,
) -> Result<Vec<DeliveredMessageId>, VivariumError> {
    let mut delivered = Vec::new();
    for recipient in recipients {
        let stored = storage.ingest_message(
            &MessageIngestRequest {
                account: recipient.clone(),
                local_role: role.to_string(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("local-delivery\0{seed}\0{recipient}"),
                remote: None,
            },
            eml.as_bytes(),
        )?;
        delivered.push(DeliveredMessageId {
            identity: recipient,
            message_id: stored.message_id,
            content_id: stored.content_id,
        });
    }
    Ok(delivered)
}

pub(super) fn delivered_with_handles(
    storage: &Storage,
    delivered: Vec<DeliveredMessageId>,
) -> Result<Vec<DeliveredMessage>, VivariumError> {
    delivered
        .into_iter()
        .map(|message| {
            Ok(DeliveredMessage {
                identity: message.identity,
                handle: storage.display_handle(&message.message_id)?,
            })
        })
        .collect()
}

pub(super) fn ingest_sent_copy(
    storage: &Storage,
    from: &str,
    eml: &str,
    seed: i64,
) -> Result<StoredMessage, VivariumError> {
    storage.ingest_message(
        &MessageIngestRequest {
            account: from.to_string(),
            local_role: "sent".into(),
            read_state: true,
            starred: false,
            message_id_hint: None,
            presented_id: None,
            seed_hint: format!("local-sent\0{seed}"),
            remote: None,
        },
        eml.as_bytes(),
    )
}

pub(super) fn collect_addresses<'a>(
    addresses: Option<&'a mail_parser::Address<'a>>,
    out: &mut BTreeSet<String>,
) {
    let Some(addresses) = addresses else {
        return;
    };
    for addr in addresses.iter() {
        if let Some(address) = addr.address.as_deref() {
            out.insert(address.to_string());
        }
    }
}

/// Warn (stderr, never blocking) when the same sender already has an open
/// item with the same subject in a recipient's work folder — the signature
/// of a re-run filing loop minting duplicates. Still-open items only:
/// resending a subject whose earlier copy settled is a normal follow-up.
pub(super) fn warn_duplicate_work_subjects(
    storage: &Storage,
    role: &str,
    subject: &str,
    from_addr: &str,
    recipients: &BTreeSet<String>,
) {
    for recipient in recipients {
        let Ok(existing) = storage.list_messages_by_account_role(recipient, role) else {
            continue;
        };
        let Some(duplicate) = existing
            .iter()
            .find(|view| view.subject == subject && view.from_addr == from_addr)
        else {
            continue;
        };
        eprintln!(
            "warning: an open {role} with subject '{subject}' from {from_addr} already exists \
             for {recipient} as {}; this send may be a duplicate",
            duplicate.handle
        );
    }
}
