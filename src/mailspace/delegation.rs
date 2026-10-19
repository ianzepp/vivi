//! Need delegation as reply letters on the immutable need.

use mail_parser::{HeaderValue, MessageParser};

use super::Mailspace;
use super::Recipient;
use super::item_events::{EventTally, HeldItem, held_item};
use crate::error::VivariumError;
use crate::message::normalize_message_id;
use crate::storage::{Storage, StoredMessageView};

const EVENT: &str = "X-Vivi-Event";
const EVENT_OF: &str = "X-Vivi-Event-Of";
const EVENT_AT: &str = "X-Vivi-Event-At";
const DELEGATE: &str = "X-Vivi-Delegate";

#[derive(Debug, Clone)]
pub(super) struct DelegationState {
    pub(super) history: bool,
    pub(super) delegate: Option<String>,
    pub(super) assigner: Option<String>,
    pub(super) assigned_at: Option<(i64, String)>,
    pub(super) lapsed_at: Option<(i64, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AssignmentEvent {
    pub(super) id: String,
    pub(super) of: String,
    pub(super) at: i64,
    pub(super) from: String,
    pub(super) delegate: Option<String>,
}

/// Parse an assignment or withdrawal reply. The letter must identify its
/// parent in both the threading header and `X-Vivi-Event-Of`.
pub(super) fn parse_assignment_event(bytes: &[u8]) -> Option<AssignmentEvent> {
    let parsed = MessageParser::default().parse(bytes)?;
    let mut kind = None;
    let mut of = None;
    let mut at = None;
    let mut delegate = None;
    for header in parsed.headers() {
        let HeaderValue::Text(text) = header.value() else {
            continue;
        };
        if header.name().eq_ignore_ascii_case(EVENT) {
            let value = text.trim().to_ascii_lowercase();
            if matches!(value.as_str(), "assigned" | "withdrawn") {
                kind = Some(value);
            }
        } else if header.name().eq_ignore_ascii_case(EVENT_OF) {
            of = normalize_message_id(text);
        } else if header.name().eq_ignore_ascii_case(EVENT_AT) {
            at = chrono::DateTime::parse_from_rfc3339(text.trim())
                .ok()
                .map(|time| time.timestamp_micros());
        } else if header.name().eq_ignore_ascii_case(DELEGATE) {
            delegate = Some(text.trim().to_ascii_lowercase());
        }
    }
    let of = of?;
    let replied = parsed
        .in_reply_to()
        .as_text()
        .and_then(normalize_message_id);
    if replied.as_deref() != Some(of.as_str()) {
        return None;
    }
    let kind = kind?;
    let delegate = if kind == "assigned" {
        let value = delegate?;
        if !value.contains('@') {
            return None;
        }
        Some(value)
    } else {
        None
    };
    Some(AssignmentEvent {
        id: parsed.message_id().and_then(normalize_message_id)?,
        of,
        at: at.unwrap_or_else(|| {
            parsed.date().map_or(0, mail_parser::DateTime::to_timestamp) * 1_000_000
        }),
        from: parsed
            .from()?
            .first()?
            .address
            .as_deref()?
            .trim()
            .to_ascii_lowercase(),
        delegate,
    })
}

/// Fold source-author assignment letters and the first close by the current
/// delegate. The close letter itself remains authorized; later events from
/// that assignment do not.
pub(super) fn delegation_state(
    storage: &Storage,
    content_id: &str,
    item_id: &str,
    item_from: &str,
) -> Result<DelegationState, VivariumError> {
    let source_domain = item_from
        .rsplit_once('@')
        .map(|(_, domain)| domain.trim().to_ascii_lowercase());
    let children = storage.reply_child_contents(content_id)?;
    let mut events = Vec::new();
    let now = super::item_events::clock_micros();
    for child in children {
        let Some(view) = storage.message_by_content_id(&child)? else {
            continue;
        };
        let Ok(bytes) = storage.read_listed_blob(&view.blob_relpath) else {
            continue;
        };
        if let Some(event) = parse_assignment_event(&bytes).filter(|event| event.of == item_id)
            && source_domain.as_deref().is_some_and(|domain| {
                super::holding::first_bare_address(&view.from_addr)
                    .and_then(|from| from.rsplit_once('@').map(|(_, domain)| domain.to_string()))
                    .is_some_and(|from_domain| from_domain.eq_ignore_ascii_case(domain))
            })
            && super::item_events::dated_within_clock(event.at, now)
        {
            events.push(event);
        }
    }
    events.sort_by(|a, b| (a.at, &a.id).cmp(&(b.at, &b.id)));
    let history = !events.is_empty();
    let latest = events.last().cloned();
    let delegate = latest.as_ref().and_then(|event| event.delegate.clone());
    let assigner = latest
        .as_ref()
        .filter(|event| event.delegate.is_some())
        .map(|event| event.from.clone());
    let assigned_at = latest
        .as_ref()
        .filter(|event| event.delegate.is_some())
        .map(|event| (event.at, event.id.clone()));
    let mut lapsed_at = None;
    if let (Some(latest), Some(delegate)) = (latest, delegate.as_deref()) {
        for child in storage.reply_child_contents(content_id)? {
            let Some(view) = storage.message_by_content_id(&child)? else {
                continue;
            };
            let Ok(bytes) = storage.read_listed_blob(&view.blob_relpath) else {
                continue;
            };
            let Some(state) = super::item_events::parse_event(&bytes) else {
                continue;
            };
            let from = super::holding::first_bare_address(&view.from_addr)
                .unwrap_or_default()
                .to_ascii_lowercase();
            if state.of == item_id
                && state.kind == super::item_events::ItemEventKind::Done
                && from == delegate
                && (state.at, &state.id) > (latest.at, &latest.id)
                && super::item_events::dated_within_clock(state.at, now)
                && lapsed_at
                    .as_ref()
                    .is_none_or(|held: &(i64, String)| (state.at, &state.id) < (held.0, &held.1))
            {
                lapsed_at = Some((state.at, state.id));
            }
        }
    }
    Ok(DelegationState {
        history,
        delegate,
        assigner,
        assigned_at,
        lapsed_at,
    })
}

impl Mailspace {
    pub(super) fn apply_assignment_projection(
        &self,
        storage: &Storage,
        item: &HeldItem,
        tally: &mut EventTally,
    ) -> Result<(), VivariumError> {
        if item.kind != "need" || self.active_delegate_identity(item).is_none() {
            return Ok(());
        }
        let Some(row) = item.rows.iter().find(|row| row.local_role == "needs") else {
            return Ok(());
        };
        let handle = storage.display_handle(&row.message_id)?;
        if super::backlog::backlog_node_of(storage, &handle)?.is_none() {
            self.backlog_attach("need", &handle, &row.subject, &[])?;
            tally.applied += 1;
        }
        Ok(())
    }

    pub(super) fn assigned_holding(
        &self,
        storage: &Storage,
        row: &StoredMessageView,
    ) -> Result<Option<super::holding::Holding>, VivariumError> {
        let Some(item) = held_item(storage, &row.content_id)? else {
            return Ok(None);
        };
        if item.delegation.lapsed_at.is_some() {
            return Ok(None);
        }
        let Some(delegate) = item.delegation.delegate.as_deref() else {
            return Ok(None);
        };
        let Some((role, domain)) = delegate.rsplit_once('@') else {
            return Ok(None);
        };
        if self.is_own_domain(domain) {
            if self.identity_owning_account(role).is_some() {
                return Ok(Some(super::holding::Holding::Local));
            }
            return Ok(Some(super::holding::Holding::Unowned(role.to_string())));
        }
        let source_domain = item
            .from_addr
            .rsplit_once('@')
            .map(|(_, domain)| domain.trim().to_ascii_lowercase());
        if source_domain
            .as_deref()
            .is_some_and(|source| self.is_own_domain(source))
        {
            return Ok(Some(super::holding::Holding::Sent(delegate.to_string())));
        }
        Ok(Some(super::holding::Holding::Foreign(row.account.clone())))
    }

    pub fn is_locally_assigned_row(
        &self,
        storage: &Storage,
        row: &StoredMessageView,
    ) -> Result<bool, VivariumError> {
        let Some(item) = held_item(storage, &row.content_id)? else {
            return Ok(false);
        };
        Ok(self.active_delegate_identity(&item).is_some())
    }

    /// Assign one need to a local role or a role in another member mailspace.
    /// Each need gets its own immutable reply letter, which lets every member
    /// fold the same assignment independently.
    pub fn assign_need(
        &self,
        identity: &str,
        handle: &str,
        delegate: &str,
    ) -> Result<String, VivariumError> {
        let identity = self.resolve_identity(identity)?;
        let delegate = self.delegate_address(delegate)?;
        if let Recipient::Local(role) = self.classify_recipient(&delegate)?
            && self.identity_is_hand(&role)
        {
            return Err(VivariumError::Message(
                "a hand-kind role cannot receive a need delegation".into(),
            ));
        }
        let storage = self.storage()?;
        let message_id = storage.resolve_message_token(handle)?;
        let view = storage
            .message_by_id(&message_id)?
            .ok_or_else(|| VivariumError::Message(format!("message not found: {handle}")))?;
        let item = held_item(&storage, &view.content_id)?
            .ok_or_else(|| VivariumError::Message(format!("{handle} is not work")))?;
        if item.kind != "need" {
            return Err(VivariumError::Message(format!(
                "{handle} is {}, not a need",
                item.kind
            )));
        }
        let assigning_role = item
            .delegation
            .assigner
            .as_deref()
            .and_then(|address| address.rsplit_once('@'))
            .filter(|(_, domain)| self.is_own_domain(domain))
            .and_then(|(role, _)| self.identity_owning_account(role));
        let may_assign = self.may_close(&item, &identity)
            || assigning_role.as_deref() == Some(identity.as_str())
            || self.identity_is_operator(&identity);
        if self.item_author(&item).is_none() || !may_assign {
            return Err(VivariumError::Message(
                "only the authoring mailspace can assign this need".into(),
            ));
        }
        let handle =
            self.emit_assignment_event(&storage, &item, &identity, Some(&delegate), &delegate)?;
        self.apply_arrived_events(std::slice::from_ref(&view.content_id))?;
        Ok(handle)
    }

    /// Withdraw the current need assignment from its authoring mailspace.
    pub fn withdraw_need(&self, identity: &str, handle: &str) -> Result<String, VivariumError> {
        let identity = self.resolve_identity(identity)?;
        let storage = self.storage()?;
        let message_id = storage.resolve_message_token(handle)?;
        let view = storage
            .message_by_id(&message_id)?
            .ok_or_else(|| VivariumError::Message(format!("message not found: {handle}")))?;
        let item = held_item(&storage, &view.content_id)?
            .ok_or_else(|| VivariumError::Message(format!("{handle} is not work")))?;
        if item.kind != "need" || item.delegation.delegate.is_none() {
            return Err(VivariumError::Message(format!(
                "{handle} has no active assignment"
            )));
        }
        let assigning_role = item
            .delegation
            .assigner
            .as_deref()
            .and_then(|address| address.rsplit_once('@'))
            .filter(|(_, domain)| self.is_own_domain(domain))
            .and_then(|(role, _)| self.identity_owning_account(role));
        if self.item_author(&item).is_none()
            || (self.item_author(&item).as_deref() != Some(identity.as_str())
                && assigning_role.as_deref() != Some(identity.as_str())
                && !self.identity_is_operator(&identity))
        {
            return Err(VivariumError::Message(
                "only the assigning mailspace can withdraw this need".into(),
            ));
        }
        let delegate = item.delegation.delegate.clone().unwrap_or_default();
        let result = self.emit_assignment_event(&storage, &item, &identity, None, &delegate)?;
        self.apply_arrived_events(std::slice::from_ref(&view.content_id))?;
        Ok(result)
    }

    fn delegate_address(&self, value: &str) -> Result<String, VivariumError> {
        Ok(match self.classify_recipient(value)? {
            Recipient::Local(role) => self.address_for(&role),
            Recipient::Remote(address) => address,
        })
    }

    /// Foreign work assigned to `identity` is listed in that role's work
    /// folder while its assignment is active.
    pub fn assigned_work_for_role(
        &self,
        identity: Option<&str>,
        role: &str,
        kind: &str,
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        let storage = self.storage()?;
        let Some(target) = identity
            .map(|identity| self.resolve_identity(identity))
            .transpose()?
        else {
            return Ok(Vec::new());
        };
        let accounts = self.identity_names(&target);
        let mut rows = Vec::new();
        for row in storage.list_messages_by_role(role)? {
            if accounts.contains(&row.account) {
                continue;
            }
            let Some(item) = held_item(&storage, &row.content_id)? else {
                continue;
            };
            if item.kind != kind || item.delegation.lapsed_at.is_some() {
                continue;
            }
            let Some(delegate) = self.active_delegate_identity(&item) else {
                continue;
            };
            if target == delegate {
                let mut row = row;
                row.account = delegate;
                rows.push(row);
            }
        }
        Ok(rows)
    }
}
