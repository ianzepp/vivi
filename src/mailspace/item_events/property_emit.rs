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
    EVENT_AT_HEADER, EVENT_HEADER, EVENT_OF_HEADER, HeldItem, Mailspace, dated_within_clock,
    parse_property_event,
};
use crate::error::VivariumError;
use crate::letter::plain::EmptyFront;
use crate::letter::{FORMAT_HEADER, FORMAT_VERSION, LetterKind, parse_kind};
use crate::mailspace::delivery::add_header;
use crate::mailspace::holding::first_bare_address;
use crate::mailspace::reply::{add_reply_headers, reply_subject};
use crate::message::{ComposeDraft, build_compose_draft};
use crate::storage::{ItemEventWrite, Storage};

/// The body of a property or assignment letter. The facts stay in the event
/// protocol headers, so the front is empty and the prose is stored as
/// written, with no fence. The body is parsed back as the event kind, so a
/// writer bug is an error before anything is stored.
fn gated_event_body(verb: &str, note: Option<&str>) -> Result<String, VivariumError> {
    let prose = note.map_or_else(|| verb.to_string(), str::to_string);
    let body = crate::letter::plain::render(&EmptyFront {}, &prose)
        .map_err(|error| VivariumError::Message(format!("the {verb} letter: {error}")))?;
    parse_kind(&LetterKind::Event(verb.to_string()), &body)
        .map_err(|error| VivariumError::Message(format!("the {verb} letter: {error}")))?;
    Ok(body)
}

/// The transport headers every property and assignment letter carries.
fn stamp_event_headers(
    data: String,
    verb: &str,
    item_id: &str,
    at: &str,
) -> Result<String, VivariumError> {
    let data = add_header(&data, "X-Vivi-Kind", "mail")?;
    let data = add_header(&data, FORMAT_HEADER, FORMAT_VERSION)?;
    let data = add_header(&data, EVENT_HEADER, verb)?;
    let data = add_header(&data, EVENT_OF_HEADER, &format!("<{item_id}>"))?;
    add_header(&data, EVENT_AT_HEADER, at)
}

/// A composed property-event letter and the local roles that get a copy.
struct PropertyLetterDraft {
    data: Vec<u8>,
    local: BTreeSet<String>,
}

impl Mailspace {
    /// Write one assignment or withdrawal reply addressed to the delegate
    /// (or former delegate), then return its local handle.
    pub(in crate::mailspace) fn emit_assignment_event(
        &self,
        storage: &Storage,
        item: &HeldItem,
        identity: &str,
        delegate: Option<&str>,
        route_to: &str,
    ) -> Result<String, VivariumError> {
        let parsed = MessageParser::default()
            .parse(&item.bytes)
            .ok_or_else(|| VivariumError::Parse("failed to parse the need".into()))?;
        let item_id = parsed
            .message_id()
            .ok_or_else(|| VivariumError::Message("the need has no Message-ID".into()))?;
        let from = self.address_for(identity);
        let (mut to, cc) = self.event_addresses(&parsed, identity, &from);
        let route = self.header_address(route_to)?;
        if !to
            .iter()
            .chain(&cc)
            .any(|address| address.eq_ignore_ascii_case(&route))
        {
            to.push(route);
        }
        let local = self.local_event_recipients(&to, &cc, identity);
        if to.is_empty() && cc.is_empty() {
            to.push(from.clone());
        }
        let at = self.property_event_at(storage, item)?;
        let verb = if delegate.is_some() {
            "assigned"
        } else {
            "withdrawn"
        };
        let mut data = build_compose_draft(&ComposeDraft {
            from,
            to,
            cc,
            bcc: Vec::new(),
            subject: reply_subject(parsed.subject().unwrap_or("(no subject)")),
            body: gated_event_body(verb, None)?,
            html_body: None,
        })?;
        data = stamp_event_headers(data, verb, item_id, &at)?;
        if let Some(delegate) = delegate {
            data = add_header(&data, "X-Vivi-Delegate", delegate)?;
        }
        data = add_reply_headers(&data, &item.bytes)?;
        self.check_letter_size(data.len())?;
        let rows = emit::event_rows(identity, &local, &item.content_id);
        let written = storage.apply_item_event(&ItemEventWrite {
            reply_requests: &rows,
            reply_data: Some(data.as_bytes()),
            parent_content_id: &item.content_id,
            moves: &[],
        })?;
        let event = written
            .last()
            .ok_or_else(|| VivariumError::Message("the assignment letter was not stored".into()))?;
        storage.display_handle(&event.message_id)
    }

    /// Compose and store one property-event letter on `item` — the write
    /// side of what `property.rs` reads — addressed like a state event,
    /// with `extras` as payload headers, and return the letter's handle.
    /// The body is `note`, else the kind word.
    pub(in crate::mailspace) fn emit_property_event(
        &self,
        storage: &Storage,
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
    /// microsecond past the newest counted state or property event this
    /// mailspace holds for the item, whichever is later — the same floor a
    /// state event is stamped with, so a writer never stamps below what it
    /// has seen or above what a reader will not count.
    fn property_event_at(
        &self,
        storage: &Storage,
        item: &HeldItem,
    ) -> Result<String, VivariumError> {
        let mut floor = super::clock_micros();
        // The chain holds counted events only; the property instants are
        // filtered here.
        for event in &self.item_chain(storage, item)?.events {
            floor = floor.max(event.at.saturating_add(1));
        }
        for at in self.held_property_ats(storage, item)? {
            floor = floor.max(at.saturating_add(1));
        }
        Ok(emit::stamp_micros(floor))
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
        let verb = kind.header();
        let mut data = build_compose_draft(&ComposeDraft {
            from,
            to,
            cc,
            bcc: Vec::new(),
            subject: reply_subject(parsed.subject().unwrap_or("(no subject)")),
            body: gated_event_body(verb, note)?,
            html_body: None,
        })?;
        data = stamp_event_headers(data, verb, item_id, at)?;
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

impl Mailspace {
    /// The instants of property and assignment events held for `item` that a
    /// member wrote and the clock still counts.
    fn held_property_ats(
        &self,
        storage: &Storage,
        item: &HeldItem,
    ) -> Result<Vec<i64>, VivariumError> {
        let now = super::clock_micros();
        let mut ats = Vec::new();
        for child in storage.reply_child_contents(&item.content_id)? {
            let Some(view) = storage.message_by_content_id(&child)? else {
                continue;
            };
            let Ok(bytes) = storage.read_listed_blob(&view.blob_relpath) else {
                continue;
            };
            let stamp = parse_property_event(&bytes)
                .map(|event| (event.of, event.at, event.id))
                .or_else(|| {
                    super::super::delegation::parse_assignment_event(&bytes)
                        .map(|event| (event.of, event.at, event.id))
                });
            let Some((_of, at, id)) = stamp.filter(|(of, _, _)| of == &item.rfc_id) else {
                continue;
            };
            if let Some(from) = first_bare_address(&view.from_addr)
                && self.event_may_speak(item, &from, at, &id)
                && dated_within_clock(at, now)
            {
                ats.push(at);
            }
        }
        Ok(ats)
    }
}

#[cfg(test)]
#[path = "property_emit_test.rs"]
mod tests;
