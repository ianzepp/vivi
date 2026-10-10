//! Project rules.
//!
//! A rule is a message in the `rules` folder. The bytes are written once.
//! Retiring one is an event. Replacing one writes a new rule whose
//! `X-Vivi-Supersedes` header names the old id, and records the same fact as
//! an event. The active set is every rule that is not retired, superseded,
//! or absorbed. Rules are not work: nothing here mints a backlog node.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use serde::Serialize;

use super::Mailspace;
use super::delivery::add_header;
use crate::error::VivariumError;
use crate::extract::extract_text;
use crate::message::{ComposeDraft, build_compose_draft};
use crate::storage::{
    MailspaceEvent, MailspaceEventInput, MessageIngestRequest, Storage, StoredMessage,
    StoredMessageView,
};

/// One rule in a list or a search result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleRecord {
    /// Display handle.
    pub handle: String,
    /// Full message id.
    pub message_id: String,
    /// Stored date.
    pub date: String,
    /// Subject line.
    pub subject: String,
    /// Identity that wrote this version.
    pub author: String,
    /// `local` or `project`.
    pub scope: String,
    /// `active`, `retired`, `superseded`, or `absorbed`.
    pub state: String,
    /// Display handle of the rule that replaced this one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub successor: Option<String>,
}

/// A rule and its body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleDetail {
    /// The rule record.
    #[serde(flatten)]
    pub rule: RuleRecord,
    /// Plain-text body.
    pub body: String,
}

/// Receipt for `rule retire`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleChange {
    /// Display handle.
    pub handle: String,
    /// Whether this call appended an event.
    pub changed: bool,
}

/// Receipt for `rule supersede`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSupersession {
    /// Display handle of the rule that left the active set.
    pub previous: String,
    /// Display handle of the new rule.
    pub handle: String,
}

struct RuleHeaders {
    scope: String,
    supersedes: Option<String>,
}

struct Disposition {
    name: &'static str,
    successor: Option<String>,
}

impl Mailspace {
    /// Save a project rule.
    ///
    /// `local` writes `X-Vivi-Rule-Scope: local`. Nothing syncs rules yet;
    /// the header is the mark a later sync will read.
    ///
    /// # Errors
    /// Returns an error when the identity is unknown or the store rejects
    /// the write.
    pub fn save_rule(
        &self,
        identity: &str,
        subject: &str,
        body: &str,
        local: bool,
    ) -> Result<String, VivariumError> {
        let identity = self.resolve_identity(identity)?;
        let stored = self.ingest_rule(&identity, subject, body, local, None)?;
        self.storage()?.display_handle(&stored.message_id)
    }

    /// Replace an active rule with a new one.
    ///
    /// The old rule stays readable and leaves the active set.
    ///
    /// # Errors
    /// Returns an error when the handle is not an active rule, the actor is
    /// unknown, or the store rejects the write.
    pub fn supersede_rule(
        &self,
        identity: &str,
        handle: &str,
        subject: &str,
        body: &str,
        local: bool,
    ) -> Result<RuleSupersession, VivariumError> {
        let actor = self.resolve_identity(identity)?;
        let previous = self.require_active(handle)?;
        let stored = self.ingest_rule(&actor, subject, body, local, Some(&previous.message_id))?;
        let storage = self.storage()?;
        storage.append_mailspace_event(&rule_event(
            "rule supersede",
            "superseded",
            &actor,
            &previous,
            Some(stored.message_id.clone()),
        ))?;
        Ok(RuleSupersession {
            previous: previous.handle,
            handle: storage.display_handle(&stored.message_id)?,
        })
    }

    /// Withdraw an active rule. A second retire appends nothing.
    ///
    /// # Errors
    /// Returns an error when the handle is not a rule, or the rule is
    /// superseded or absorbed.
    pub fn retire_rule(
        &self,
        identity: &str,
        handle: &str,
        note: Option<&str>,
    ) -> Result<RuleChange, VivariumError> {
        let actor = self.resolve_identity(identity)?;
        let message = self.load_rule(handle)?;
        let state = self.disposition(&message)?;
        if state.name == "retired" {
            return Ok(RuleChange {
                handle: message.handle,
                changed: false,
            });
        }
        if state.name != "active" {
            return Err(refuse_inactive(
                state.name,
                state.successor.as_deref(),
                handle,
            ));
        }
        let storage = self.storage()?;
        storage.append_mailspace_event(&rule_event(
            "rule retire",
            "retired",
            &actor,
            &message,
            note.map(str::to_string),
        ))?;
        Ok(RuleChange {
            handle: message.handle,
            changed: true,
        })
    }

    /// Seal a rule. A second absorb appends nothing.
    ///
    /// Any project identity may absorb. The rule is not privately held.
    ///
    /// # Errors
    /// Returns an error when the handle is not a rule or the actor is unknown.
    pub fn absorb_rule(
        &self,
        identity: &str,
        handle: &str,
        note: Option<&str>,
    ) -> Result<String, VivariumError> {
        let actor = self.resolve_identity(identity)?;
        let message = self.load_rule(handle)?;
        let mut storage = self.storage()?;
        let newly = storage.absorb_message(&message.account, &message.message_id, &actor)?;
        if newly {
            storage.append_mailspace_event(&rule_event(
                "rule absorb",
                "absorbed",
                &actor,
                &message,
                note.map(str::to_string),
            ))?;
        }
        Ok(message.handle)
    }

    /// Active rules, or every rule when `history` is set.
    ///
    /// `author` limits the list to rules that identity wrote. The rows are
    /// ordered by subject, then handle.
    ///
    /// # Errors
    /// Returns an error when the author is unknown or the store cannot be read.
    pub fn list_rules(
        &self,
        author: Option<&str>,
        history: bool,
    ) -> Result<Vec<RuleRecord>, VivariumError> {
        self.collect_rules(author, history)
    }

    /// Rules whose subject or body contains `query`.
    ///
    /// The match is case-insensitive. `subject_only` skips the body.
    /// `history` includes retired, superseded, and absorbed rules.
    ///
    /// # Errors
    /// Returns an error when the author is unknown or a message cannot be read.
    pub fn search_rules(
        &self,
        query: &str,
        author: Option<&str>,
        subject_only: bool,
        history: bool,
    ) -> Result<Vec<RuleRecord>, VivariumError> {
        let query = query.to_lowercase();
        let records = self.collect_rules(author, history)?;
        if subject_only {
            return Ok(records
                .into_iter()
                .filter(|rule| rule.subject.to_lowercase().contains(&query))
                .collect());
        }
        self.rules_matching_body(records, &query)
    }

    /// One rule, including a rule that has left the active set.
    ///
    /// # Errors
    /// Returns an error when the handle is not a rule or the body cannot be read.
    pub fn show_rule(&self, handle: &str) -> Result<RuleDetail, VivariumError> {
        let message = self.load_rule(handle)?;
        let pointed = self.successor_pointing_at(&message.message_id)?;
        let storage = self.storage()?;
        let events = storage.list_mailspace_events(&message.message_id)?;
        let rule = record_for(&storage, &message, &events, pointed)?;
        let data = storage.read_message(&message.message_id)?;
        Ok(RuleDetail {
            rule,
            body: extract_text(&data)?,
        })
    }
}

impl Mailspace {
    fn ingest_rule(
        &self,
        identity: &str,
        subject: &str,
        body: &str,
        local: bool,
        supersedes: Option<&str>,
    ) -> Result<StoredMessage, VivariumError> {
        let address = self.address_for(identity);
        let mut eml = build_compose_draft(&ComposeDraft {
            from: address.clone(),
            to: vec![address],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.to_string(),
            body: body.to_string(),
            html_body: None,
        })?;
        eml = add_header(&eml, "X-Vivi-Kind", "rule")?;
        let scope = if local { "local" } else { "project" };
        eml = add_header(&eml, "X-Vivi-Rule-Scope", scope)?;
        if let Some(previous) = supersedes {
            eml = add_header(&eml, "X-Vivi-Supersedes", previous)?;
        }
        let seed = Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let prior = supersedes.unwrap_or("");
        self.storage()?.ingest_message(
            &MessageIngestRequest {
                account: identity.to_string(),
                local_role: "rules".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("rule-save\0{seed}\0{identity}\0{prior}"),
                remote: None,
            },
            eml.as_bytes(),
        )
    }

    fn require_active(&self, token: &str) -> Result<StoredMessageView, VivariumError> {
        let message = self.load_rule(token)?;
        let state = self.disposition(&message)?;
        if state.name == "active" {
            Ok(message)
        } else {
            Err(refuse_inactive(
                state.name,
                state.successor.as_deref(),
                token,
            ))
        }
    }

    fn load_rule(&self, token: &str) -> Result<StoredMessageView, VivariumError> {
        let storage = self.storage()?;
        let resolved = storage.resolve_message_token(token)?;
        let Some(message) = storage.message_by_id(&resolved)? else {
            return Err(VivariumError::Message(format!(
                "message not found: {token}"
            )));
        };
        let kind = self.source_kind(&storage, &message)?;
        if kind == "rule" {
            Ok(message)
        } else {
            Err(VivariumError::Message(format!("{token} is not a rule")))
        }
    }

    fn disposition(&self, message: &StoredMessageView) -> Result<Disposition, VivariumError> {
        let storage = self.storage()?;
        let events = storage.list_mailspace_events(&message.message_id)?;
        let successor_id = successor_note(&events)
            .map(str::to_string)
            .or(self.successor_pointing_at(&message.message_id)?);
        let name = state_name(
            &events,
            message.absorbed_at.is_some(),
            successor_id.is_some(),
        );
        let successor = successor_id
            .map(|id| storage.display_handle(&id))
            .transpose()?;
        Ok(Disposition { name, successor })
    }

    fn successor_pointing_at(&self, message_id: &str) -> Result<Option<String>, VivariumError> {
        let storage = self.storage()?;
        for rule in self.list_kind(None, "rules", "rule")? {
            let data = storage.read_message(&rule.message_id)?;
            if rule_headers(&data).supersedes.as_deref() == Some(message_id) {
                return Ok(Some(rule.message_id));
            }
        }
        Ok(None)
    }

    fn collect_rules(
        &self,
        author: Option<&str>,
        history: bool,
    ) -> Result<Vec<RuleRecord>, VivariumError> {
        let names = author_names(self, author)?;
        let rules = self.list_kind(None, "rules", "rule")?;
        let storage = self.storage()?;
        let ids = rules
            .iter()
            .map(|rule| rule.message_id.clone())
            .collect::<Vec<_>>();
        let events = storage.list_mailspace_events_for_messages(&ids)?;
        let pointed = successor_index(&storage, &rules)?;
        let mut records = Vec::new();
        for rule in rules {
            if !author_ok(names.as_ref(), &rule.account) {
                continue;
            }
            let rule_events = events.get(&rule.message_id).map_or(&[][..], Vec::as_slice);
            let successor = pointed.get(&rule.message_id).cloned();
            let record = record_for(&storage, &rule, rule_events, successor)?;
            if history || record.state == "active" {
                records.push(record);
            }
        }
        records.sort_by(|left, right| {
            left.subject
                .cmp(&right.subject)
                .then(left.handle.cmp(&right.handle))
        });
        Ok(records)
    }

    fn rules_matching_body(
        &self,
        records: Vec<RuleRecord>,
        query: &str,
    ) -> Result<Vec<RuleRecord>, VivariumError> {
        let storage = self.storage()?;
        let mut matched = Vec::new();
        for record in records {
            if record.subject.to_lowercase().contains(query) {
                matched.push(record);
                continue;
            }
            let data = storage.read_message(&record.message_id)?;
            if extract_text(&data)?.to_lowercase().contains(query) {
                matched.push(record);
            }
        }
        Ok(matched)
    }
}

fn author_names(
    mailspace: &Mailspace,
    author: Option<&str>,
) -> Result<Option<HashSet<String>>, VivariumError> {
    let Some(author) = author else {
        return Ok(None);
    };
    let identity = mailspace.resolve_identity(author)?;
    Ok(Some(mailspace.identity_names(&identity)))
}

fn author_ok(names: Option<&HashSet<String>>, account: &str) -> bool {
    names.is_none_or(|names| names.contains(account))
}

fn record_for(
    storage: &Storage,
    rule: &StoredMessageView,
    events: &[MailspaceEvent],
    pointed_successor: Option<String>,
) -> Result<RuleRecord, VivariumError> {
    let data = storage.read_message(&rule.message_id)?;
    let successor_id = successor_note(events)
        .map(str::to_string)
        .or(pointed_successor);
    let state = state_name(events, rule.absorbed_at.is_some(), successor_id.is_some());
    let successor = successor_id
        .map(|id| storage.display_handle(&id))
        .transpose()?;
    Ok(RuleRecord {
        handle: rule.handle.clone(),
        message_id: rule.message_id.clone(),
        date: rule.date.clone(),
        subject: rule.subject.clone(),
        author: rule.account.clone(),
        scope: rule_headers(&data).scope,
        state: state.to_string(),
        successor,
    })
}

fn successor_index(
    storage: &Storage,
    rules: &[StoredMessageView],
) -> Result<HashMap<String, String>, VivariumError> {
    let mut index = HashMap::new();
    for rule in rules {
        let data = storage.read_message(&rule.message_id)?;
        if let Some(target) = rule_headers(&data).supersedes {
            index.insert(target, rule.message_id.clone());
        }
    }
    Ok(index)
}

fn rule_event(
    command: &str,
    event_type: &str,
    actor: &str,
    message: &StoredMessageView,
    note: Option<String>,
) -> MailspaceEventInput {
    MailspaceEventInput {
        command: command.to_string(),
        event_type: event_type.to_string(),
        actor_identity: Some(actor.to_string()),
        account: message.account.clone(),
        message_id: message.message_id.clone(),
        content_id: message.content_id.clone(),
        from_role: Some(message.local_role.clone()),
        to_role: Some(message.local_role.clone()),
        from_identity: None,
        to_identity: Some(message.account.clone()),
        subject: message.subject.clone(),
        note,
    }
}

fn state_name(events: &[MailspaceEvent], absorbed: bool, pointed: bool) -> &'static str {
    if pointed || events.iter().any(|event| event.event_type == "superseded") {
        return "superseded";
    }
    if events.iter().any(|event| event.event_type == "retired") {
        return "retired";
    }
    if absorbed { "absorbed" } else { "active" }
}

fn successor_note(events: &[MailspaceEvent]) -> Option<&str> {
    events
        .iter()
        .rev()
        .find(|event| event.event_type == "superseded")
        .and_then(|event| event.note.as_deref())
}

fn rule_headers(data: &[u8]) -> RuleHeaders {
    let mut headers = RuleHeaders {
        scope: "project".to_string(),
        supersedes: None,
    };
    let Ok(raw) = std::str::from_utf8(data) else {
        return headers;
    };
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("X-Vivi-Rule-Scope") && value == "local" {
            headers.scope = "local".to_string();
        } else if name.eq_ignore_ascii_case("X-Vivi-Supersedes") && !value.is_empty() {
            headers.supersedes = Some(value.to_string());
        }
    }
    headers
}

fn refuse_inactive(name: &str, successor: Option<&str>, token: &str) -> VivariumError {
    let message = match name {
        "retired" => format!("{token} is retired"),
        "superseded" => format!(
            "{token} is superseded by {}",
            successor.unwrap_or("a newer rule")
        ),
        "absorbed" => format!("{token} is absorbed and can no longer be changed"),
        _ => format!("{token} is not active"),
    };
    VivariumError::Message(message)
}

#[cfg(test)]
#[path = "rules_test.rs"]
mod tests;
