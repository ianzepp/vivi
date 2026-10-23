//! The computed state of a want. Nothing is stored: every read folds the
//! needs that serve the want.
//!
//! The rule: a want with at least one serving need is `done` when all of them
//! are done, else `open`, and a hand close of the want is ignored (so a
//! later serving need wins). A want with no serving needs is `done` only when
//! it was closed by hand. A need that only `explores` the want never counts
//! (ruling W6): its key is never read here.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use super::Mailspace;
use super::backlog::canonical_content_handle;
use super::item_facts::{held_item, is_done};
use super::kind::effective_kind;
use crate::error::VivariumError;
use crate::storage::{Storage, StoredMessageView};

/// Whether a want still has work to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WantState {
    Open,
    Done,
}

impl WantState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Done => "done",
        }
    }
}

/// One need that serves a want, as the want's state sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServingNeed {
    pub handle: String,
    pub subject: String,
    pub done: bool,
    pub verdict: Option<String>,
}

impl ServingNeed {
    #[must_use]
    pub fn state(&self) -> WantState {
        if self.done {
            WantState::Done
        } else {
            WantState::Open
        }
    }
}

/// A want's computed state with the facts behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WantStateReport {
    pub state: WantState,
    pub serving: Vec<ServingNeed>,
    pub closed_by_hand: bool,
}

impl WantStateReport {
    /// One line: the state and what it follows.
    #[must_use]
    pub fn summary(&self) -> String {
        let state = self.state.as_str();
        if self.serving.is_empty() {
            return if self.closed_by_hand {
                format!("{state} (closed by hand; no serving needs)")
            } else {
                format!("{state} (no serving needs)")
            };
        }
        let done = self.serving.iter().filter(|need| need.done).count();
        format!(
            "{state} ({done} of {} serving needs done)",
            self.serving.len()
        )
    }
}

/// Fold the serving needs' done flags and the hand close into a state.
#[must_use]
pub fn fold(closed_by_hand: bool, serving_done: impl IntoIterator<Item = bool>) -> WantState {
    let mut any = false;
    let mut all_done = true;
    for done in serving_done {
        any = true;
        all_done &= done;
    }
    let done = if any { all_done } else { closed_by_hand };
    if done {
        WantState::Done
    } else {
        WantState::Open
    }
}

/// The needs serving the want whose normalized Message-ID is `want_rfc_id`,
/// one per content id, oldest first.
pub(crate) fn serving_needs(
    storage: &Storage,
    want_rfc_id: &str,
) -> Result<Vec<ServingNeed>, VivariumError> {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for message_id in storage.rows_with_item_metadata("serves", want_rfc_id)? {
        let Some(row) = storage.message_by_id(&message_id)? else {
            continue;
        };
        if row.absorbed_at.is_some() || !seen.insert(row.content_id.clone()) {
            continue;
        }
        let item = held_item(storage, &row.message_id)?;
        let mut verdict = None;
        for held in &item.rows {
            let metadata = storage.item_metadata(&held.message_id)?;
            verdict = metadata.get("verdict").filter(|v| !v.is_empty()).cloned();
            if verdict.is_some() {
                break;
            }
        }
        let handle = canonical_content_handle(storage, &row.message_id)?;
        let need = ServingNeed {
            handle,
            subject: row.subject.clone(),
            done: is_done(&item),
            verdict,
        };
        found.push((row.date, need));
    }
    found.sort_by(|a, b| (&a.0, &a.1.handle).cmp(&(&b.0, &b.1.handle)));
    Ok(found.into_iter().map(|(_, need)| need).collect())
}

/// The computed state of every want in `wants`, keyed by content id.
pub(crate) fn want_states(
    storage: &Storage,
    wants: &[StoredMessageView],
) -> Result<HashMap<String, WantStateReport>, VivariumError> {
    let mut reports = HashMap::new();
    for want in wants {
        if reports.contains_key(&want.content_id) {
            continue;
        }
        reports.insert(want.content_id.clone(), report_for(storage, want)?);
    }
    Ok(reports)
}

fn report_for(
    storage: &Storage,
    want: &StoredMessageView,
) -> Result<WantStateReport, VivariumError> {
    let serving = match &want.normalized_message_id {
        Some(id) => serving_needs(storage, id)?,
        None => Vec::new(),
    };
    let closed_by_hand = is_done(&held_item(storage, &want.message_id)?);
    let state = fold(closed_by_hand, serving.iter().map(|need| need.done));
    Ok(WantStateReport {
        state,
        serving,
        closed_by_hand,
    })
}

/// Refuse a hand close of a want that serving needs decide.
pub(crate) fn refuse_hand_close(
    storage: &Storage,
    want: &StoredMessageView,
) -> Result<(), VivariumError> {
    let Some(id) = &want.normalized_message_id else {
        return Ok(());
    };
    let serving = serving_needs(storage, id)?;
    if serving.is_empty() {
        return Ok(());
    }
    let shown = canonical_content_handle(storage, &want.message_id)?;
    let needs = serving
        .iter()
        .map(|need| need.handle.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(VivariumError::Message(format!(
        "want {shown} is served by needs {needs}: its state follows them"
    )))
}

impl Mailspace {
    /// The computed state of the want `handle` names.
    ///
    /// # Errors
    /// Returns an error when the handle does not resolve, names an item that
    /// is not a want, or the mailspace cannot be read.
    pub fn want_state(&self, handle: &str) -> Result<WantStateReport, VivariumError> {
        let storage = self.storage()?;
        let row = storage.resolve_message_token(handle)?;
        let Some(view) = storage.message_by_id(&row)? else {
            return Err(VivariumError::Message(format!(
                "message not found: {handle}"
            )));
        };
        let data = storage.read_message(&row)?;
        let events = storage.list_mailspace_events(&row)?;
        let kind =
            effective_kind(&view.local_role, &data, &events).unwrap_or_else(|| "mail".into());
        if kind != "want" {
            return Err(VivariumError::Message(format!(
                "{handle} is a {kind}, not a want"
            )));
        }
        report_for(&storage, &view)
    }

    /// The computed state of each want in `wants`, keyed by content id.
    ///
    /// # Errors
    /// Returns an error when the mailspace cannot be read.
    pub fn want_states_of(
        &self,
        wants: &[StoredMessageView],
    ) -> Result<HashMap<String, WantStateReport>, VivariumError> {
        let storage = self.storage()?;
        want_states(&storage, wants)
    }
}

#[cfg(test)]
#[path = "want_state_test.rs"]
mod tests;
