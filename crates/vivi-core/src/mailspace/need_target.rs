//! A need's repository target: resolved when the need is sent, and changeable
//! afterwards with a retarget letter.
//!
//! The targets a need may name are the project's `[pump.targets.<id>]` keys,
//! or the implicit `default` target of a pump with no target tables; a project
//! with no `[pump]` table has none. A target the sender names must be one of
//! them, else the send is refused before anything is stored. A send that names
//! none takes the pump's primary target (`PumpConfig::primary_target`); a
//! project with several targets and no primary refuses the send, and one with
//! no explicit targets stores no target at all rather than inventing the
//! literal `default`.
//!
//! A sent need keeps its stored target. [`Mailspace::retarget_need`] changes
//! it afterwards with a `retargeted` property letter, the same machinery marks
//! and dependencies use, so the change travels like any other event and the
//! newest retarget is the target [`Mailspace::need_target`] reports.

use std::collections::BTreeSet;

use serde::Serialize;

use super::Mailspace;
use super::item_events::{HeldItem, PropertyKind, held_item};
use vivi_error::VivariumError;

/// The header a retarget letter names its target in.
pub(super) const EVENT_TARGET_HEADER: &str = "X-Vivi-Event-Target";

/// One need's target after a `retarget` command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NeedRetarget {
    /// Display handle of the need.
    pub handle: String,
    /// The target the need reports now.
    pub target: String,
    /// The target the need carried before, when it named one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    /// The retarget letter's handle; absent when the need already had the
    /// target and nothing was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// Whether this command moved the need.
    pub changed: bool,
}

/// The repository targets a need may name in `config`: the pump's target
/// keys, the implicit `default` target when the pump sets none, and nothing
/// for a project without a `[pump]` table. The same set a role's `target`
/// may name.
#[must_use]
pub fn need_target_ids(config: &super::MailspaceConfig) -> BTreeSet<String> {
    crate::routing::role_target_ids(config)
}

/// The target a need is stored with at send time: the target the sender
/// named, when it named one, else the project's primary target.
///
/// `Ok(None)` means the need is stored with no target: the project configures
/// no targets at all, or only the legacy implicit one, which a need never
/// writes as a literal.
///
/// # Errors
/// Returns an error when the named target is not one the project configures,
/// or when no target was named and the project has several targets with no
/// primary. The error lists the targets.
pub fn resolve_send_target(
    config: &super::MailspaceConfig,
    named: Option<&str>,
) -> Result<Option<String>, VivariumError> {
    let ids = need_target_ids(config);
    let named = named.map(str::trim).filter(|target| !target.is_empty());
    let Some(named) = named else {
        return primary_target(config, &ids);
    };
    if ids.contains(named) {
        return Ok(Some(named.to_string()));
    }
    Err(VivariumError::Message(format!(
        "unknown target \"{named}\"; {}",
        target_list(&ids)
    )))
}

/// Refuse a target the project does not configure, for `command`.
///
/// # Errors
/// Returns an error naming the valid targets when `target` is not one.
pub(super) fn require_target(
    config: &super::MailspaceConfig,
    target: &str,
    command: &str,
) -> Result<String, VivariumError> {
    let ids = need_target_ids(config);
    let target = target.trim();
    if target.is_empty() {
        return Err(VivariumError::Message(format!(
            "{command} names no target; {}",
            target_list(&ids)
        )));
    }
    if ids.contains(target) {
        return Ok(target.to_string());
    }
    Err(VivariumError::Message(format!(
        "{command} names unknown target \"{target}\"; {}",
        target_list(&ids)
    )))
}

/// The pump's primary target when the project has one. The legacy implicit
/// target is not written as a literal: with no explicit targets the need
/// keeps no target, as it did before targets existed.
fn primary_target(
    config: &super::MailspaceConfig,
    ids: &BTreeSet<String>,
) -> Result<Option<String>, VivariumError> {
    let Some(pump) = config.pump.as_ref() else {
        return Ok(None);
    };
    if pump.targets.is_empty() {
        return Ok(None);
    }
    match pump.primary_target() {
        Some(target) if ids.contains(&target) => Ok(Some(target)),
        _ => Err(VivariumError::Message(format!(
            "the need names no target and the project has {} targets with no primary; \
             name one in the front matter's `target`",
            target_list(ids)
        ))),
    }
}

/// The valid targets as an error fragment: their list, or what to do when
/// there are none.
pub(super) fn target_list(ids: &BTreeSet<String>) -> String {
    if ids.is_empty() {
        return String::from(
            "this project configures no repository targets; add [pump.targets.<id>] to \
             .vivi/mailspace.toml",
        );
    }
    format!(
        "the project's targets are {}",
        ids.iter().cloned().collect::<Vec<_>>().join(", ")
    )
}

impl Mailspace {
    /// The target the need `handle` names reports now: the newest counted
    /// retarget letter, else the target its front matter was sent with. A
    /// stored `default` or a missing target stays what it is until it is
    /// retargeted; history is never rewritten. `None` for an item that is not
    /// a need, or a need that names no target.
    ///
    /// # Errors
    /// Returns an error when the handle does not resolve or storage fails.
    pub fn need_target(&self, handle: &str) -> Result<Option<String>, VivariumError> {
        let storage = self.storage()?;
        let id = storage.resolve_message_token(handle)?;
        let Some(view) = storage.message_by_id(&id)? else {
            return Ok(None);
        };
        let Some(item) = held_item(&storage, &view.content_id)? else {
            return Ok(None);
        };
        if item.kind != "need" {
            return Ok(None);
        }
        if let Some(target) = super::item_events::retarget_target(self, &storage, &item)? {
            return Ok(Some(target));
        }
        Ok(sent_target(&item))
    }

    /// Change the need `handle` targets, as `identity` (the need's author when
    /// `None`): validate the target against the project's targets, then write
    /// one `retargeted` property letter carrying it. The newest retarget
    /// wins, so every member that holds the letters reports the same target.
    /// A target the need already has writes nothing.
    ///
    /// # Errors
    /// Returns an error when the target is not one the project configures,
    /// the handle is not an open need, or `identity` may not close it (only
    /// its author, a mind, or an operator may).
    pub fn retarget_need(
        &self,
        identity: Option<&str>,
        handle: &str,
        target: &str,
        reason: Option<&str>,
    ) -> Result<NeedRetarget, VivariumError> {
        let target = require_target(&self.config, target, "need retarget")?;
        let storage = self.storage()?;
        let id = storage.resolve_message_token(handle)?;
        let Some(view) = storage.message_by_id(&id)? else {
            return Err(VivariumError::Message(format!(
                "message not found: {handle}"
            )));
        };
        let Some(item) = held_item(&storage, &view.content_id)? else {
            return Err(VivariumError::Message(format!("{handle} is not held work")));
        };
        if item.kind != "need" {
            return Err(VivariumError::Message(format!("{handle} is not a need")));
        }
        let actor = self.retarget_actor(identity, &item, handle)?;
        let shown = storage.display_handle(&view.message_id)?;
        let previous = self.need_target(handle)?;
        if previous.as_deref() == Some(target.as_str()) {
            return Ok(NeedRetarget {
                handle: shown,
                target,
                previous,
                event: None,
                changed: false,
            });
        }
        if view.absorbed_at.is_some() {
            return Err(VivariumError::Message(format!(
                "{handle} is absorbed and can no longer be changed"
            )));
        }
        if !self.may_close(&item, &actor) {
            return Err(VivariumError::Message(format!(
                "{actor} may not retarget {handle}: only its author, a mind, or an operator may"
            )));
        }
        let extras = [(EVENT_TARGET_HEADER, target.clone())];
        let event = self.emit_property_event(
            &storage,
            &item,
            &actor,
            PropertyKind::Retargeted,
            &extras,
            reason,
        )?;
        Ok(NeedRetarget {
            handle: shown,
            target,
            previous,
            event: Some(event),
            changed: true,
        })
    }

    /// The role a retarget runs as: `identity` when named, else the need's
    /// own author, which every mailspace role that wrote the need may do.
    fn retarget_actor(
        &self,
        identity: Option<&str>,
        item: &HeldItem,
        handle: &str,
    ) -> Result<String, VivariumError> {
        if let Some(identity) = identity {
            return self.resolve_identity(identity);
        }
        self.resolve_identity(&item.from_addr).map_err(|_| {
            VivariumError::Message(format!(
                "{handle} was sent by {}, which is not a role of this mailspace; \
                 pass --for <identity>",
                if item.from_addr.is_empty() {
                    "an unknown sender"
                } else {
                    item.from_addr.as_str()
                }
            ))
        })
    }
}

/// The target the need's own letter was sent with, read from its front
/// matter. A legacy letter, or one that names no target, has none.
fn sent_target(item: &HeldItem) -> Option<String> {
    let Ok(text) = vivi_util::extract::extract_text(&item.bytes) else {
        return None;
    };
    let Ok(envelope) = vivi_mail::letter::read::need_of(&item.rfc_id, &text) else {
        return None;
    };
    envelope
        .front
        .target
        .filter(|target| !target.trim().is_empty())
}

#[cfg(test)]
#[path = "need_target_test.rs"]
mod tests;
