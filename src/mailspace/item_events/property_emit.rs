//! Writing a property event: the letter an absorb seals with.
//!
//! [`Mailspace::absorb`] writes an `absorbed` letter when the role that
//! seals may close the item for everyone: composed and stored exactly the
//! way a state-event letter is, so it travels through sync like any letter
//! and every member that holds the item seals it in the state it names.

use std::collections::BTreeSet;

use mail_parser::MessageParser;

use super::emit;
use super::property::PropertyKind;
use super::{
    EVENT_AT_HEADER, EVENT_HEADER, EVENT_OF_HEADER, HeldItem, Mailspace, parse_property_event,
};
use crate::error::VivariumError;
use crate::mailspace::delivery::add_header;
use crate::mailspace::reply::{add_reply_headers, reply_subject};
use crate::message::{ComposeDraft, build_compose_draft};
use crate::storage::{ItemEventWrite, Storage};

/// A composed property-event letter and the local roles that get a copy.
struct PropertyLetterDraft {
    data: Vec<u8>,
    local: BTreeSet<String>,
}

impl Mailspace {
    /// Compose and store one property-event letter on `item` — the write
    /// side of what `property.rs` reads — addressed like a state event,
    /// with `extras` as payload headers, and return the letter's handle.
    /// The body is `note`, else the kind word.
    pub(in crate::mailspace) fn emit_property_event(
        &self,
        storage: &mut Storage,
        item: &HeldItem,
        identity: &str,
        kind: PropertyKind,
        extras: &[(&str, String)],
        note: Option<&str>,
    ) -> Result<String, VivariumError> {
        let at = self.property_event_at(storage, item)?;
        let letter = self.compose_property_letter(item, identity, kind, extras, note, &at)?;
        self.check_letter_size(letter.data.len())?;
        let rows = emit::event_rows(identity, &letter.local, &item.content_id);
        let written = storage.apply_item_event(&ItemEventWrite {
            reply_requests: &rows,
            reply_data: Some(&letter.data),
            parent_content_id: &item.content_id,
            moves: &[],
        })?;
        // The last row is the writer's own sent copy of the letter.
        let event = written
            .last()
            .ok_or_else(|| VivariumError::Message("the event letter was not stored".into()))?;
        storage.display_handle(&event.message_id)
    }

    /// The `X-Vivi-Event-At` of a new property event: the clock, or one
    /// microsecond past the newest state or property event this mailspace
    /// holds for the item, whichever is later — the same floor a state
    /// event is stamped with, so a writer never stamps below what it holds.
    fn property_event_at(
        &self,
        storage: &Storage,
        item: &HeldItem,
    ) -> Result<String, VivariumError> {
        let mut floor = chrono::Utc::now().timestamp_micros();
        for event in &self.item_chain(storage, item)?.events {
            floor = floor.max(event.at.saturating_add(1));
        }
        for at in held_property_ats(storage, item)? {
            floor = floor.max(at.saturating_add(1));
        }
        Ok(stamp_micros(floor))
    }

    /// The letter itself: the item's From, To and Cc minus the sender, the
    /// kind and payload headers, and the reply headers of the item.
    fn compose_property_letter(
        &self,
        item: &HeldItem,
        identity: &str,
        kind: PropertyKind,
        extras: &[(&str, String)],
        note: Option<&str>,
        at: &str,
    ) -> Result<PropertyLetterDraft, VivariumError> {
        let parsed = MessageParser::default()
            .parse(&item.bytes)
            .ok_or_else(|| VivariumError::Parse("failed to parse the item".into()))?;
        let item_id = parsed
            .message_id()
            .ok_or_else(|| VivariumError::Message("the item has no Message-ID".into()))?;
        let from = self.address_for(identity);
        let (mut to, cc) = self.event_addresses(&parsed, identity, &from);
        let local = self.local_event_recipients(&to, &cc, identity);
        if to.is_empty() && cc.is_empty() {
            // Nobody else is on the item: the letter is the writer's own
            // record, and a restored checkout of the mailspace reads it back.
            to.push(from.clone());
        }
        let body = note.map_or_else(|| kind.header().to_string(), str::to_string);
        let mut data = build_compose_draft(&ComposeDraft {
            from,
            to,
            cc,
            bcc: Vec::new(),
            subject: reply_subject(parsed.subject().unwrap_or("(no subject)")),
            body,
            html_body: None,
        })?;
        data = add_header(&data, "X-Vivi-Kind", "mail")?;
        data = add_header(&data, EVENT_HEADER, kind.header())?;
        data = add_header(&data, EVENT_OF_HEADER, &format!("<{item_id}>"))?;
        data = add_header(&data, EVENT_AT_HEADER, at)?;
        for (name, value) in extras {
            data = add_header(&data, name, value)?;
        }
        data = add_reply_headers(&data, &item.bytes)?;
        Ok(PropertyLetterDraft {
            data: data.into_bytes(),
            local,
        })
    }
}

/// The instants of the property events held for `item` that a member wrote.
fn held_property_ats(storage: &Storage, item: &HeldItem) -> Result<Vec<i64>, VivariumError> {
    let mut ats = Vec::new();
    for child in storage.reply_child_contents(&item.content_id)? {
        let Some(view) = storage.message_by_content_id(&child)? else {
            continue;
        };
        let Ok(bytes) = storage.read_listed_blob(&view.blob_relpath) else {
            continue;
        };
        let Some(event) = parse_property_event(&bytes).filter(|event| event.of == item.rfc_id)
        else {
            continue;
        };
        if Mailspace::event_speaks_for(&event.from_domain) {
            ats.push(event.at);
        }
    }
    Ok(ats)
}

/// An instant in Unix microseconds as RFC 3339 UTC with microseconds.
fn stamp_micros(micros: i64) -> String {
    chrono::DateTime::from_timestamp_micros(micros)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}
