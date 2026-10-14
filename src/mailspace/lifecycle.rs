use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::item_events::{
    HeldItem, ItemEventKind, PropertyKind, dated_within_clock, parse_event, parse_property_event,
};
use super::kind::effective_kind;
use super::{DeliveryResult, Mailspace, SendRequest, prefix_of_domain};
use crate::error::VivariumError;
use crate::storage::{MailspaceEvent, MailspaceEventInput, Storage, StoredMessageView};

/// Absorb ruling settings (the `[absorb]` table in `mailspace.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbsorbConfig {
    /// How long the newest event held for an item must have been the last
    /// one before a role that may close it absorbs it. `0` turns the wait
    /// off.
    #[serde(default = "default_quiet_hours")]
    pub quiet_hours: u32,
}

/// The quiet period when `[absorb]` names none: 24 hours.
const DEFAULT_QUIET_HOURS: u32 = 24;

/// One hour in Unix microseconds.
const HOUR_MICROS: i64 = 3_600_000_000;

fn default_quiet_hours() -> u32 {
    DEFAULT_QUIET_HOURS
}

impl AbsorbConfig {
    /// The quiet period in hours: the `[absorb]` value, else the default.
    #[must_use]
    pub fn quiet_hours_of(config: Option<&Self>) -> u32 {
        config.map_or(DEFAULT_QUIET_HOURS, |absorb| absorb.quiet_hours)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MailAbsorbFilter {
    #[default]
    All,
    Absorbed,
    Unabsorbed,
}

#[derive(Debug, Clone)]
pub struct SourceTaskRequest {
    pub source_handle: String,
    pub actor: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceTaskResult {
    pub source_handle: String,
    pub source_kind: String,
    pub delivered: Vec<super::DeliveredMessage>,
    pub sent: String,
    /// Recipients in other mailspaces, reached by sync.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remote: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct WantMetadataUpdate {
    pub priority: String,
    pub rank: Option<i64>,
    pub repo: Option<String>,
    pub lane: Option<String>,
    pub blocks_claim: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct WantListOptions {
    pub repo: Option<String>,
    pub lane: Option<String>,
    pub sort: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WantListRecord {
    pub handle: String,
    pub kind: String,
    pub status: String,
    pub role: String,
    pub date: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub metadata: BTreeMap<String, String>,
    /// Folded mark slugs, sorted.
    pub marks: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CycleIntake {
    pub cursor: i64,
    pub next_cursor: i64,
    pub unabsorbed_mail: Vec<super::dump::DumpRecord>,
    pub completed_tasks: Vec<MailspaceEvent>,
    pub open_needs: Vec<super::dump::DumpRecord>,
    pub open_wants: Vec<WantListRecord>,
}

impl Mailspace {
    /// Absorb a mailspace record. After absorb, the record cannot be changed.
    ///
    /// # Errors
    /// Returns an error if the identity or message cannot be resolved, if the
    /// record is not the expected kind, or if a storage operation fails.
    pub fn absorb(
        &self,
        identity: &str,
        handle: &str,
        note: Option<&str>,
        expected_kind: &str,
    ) -> Result<String, VivariumError> {
        let (identity, message) = self.resolve_owned_message(identity, handle)?;
        let storage = self.storage()?;
        let kind = self.source_kind(&storage, &message)?;
        if kind != expected_kind {
            return Err(VivariumError::Message(format!(
                "{handle} is {kind}, not {expected_kind}"
            )));
        }
        if message.absorbed_at.is_none() && matches!(expected_kind, "need" | "task" | "want") {
            self.require_quiet_absorb(&storage, &identity, &message, handle)?;
        }
        let mut storage = storage;
        let newly = storage.absorb_message(&message.account, &message.message_id, &identity)?;
        if newly {
            storage.append_mailspace_event(&absorb_event(
                expected_kind,
                &identity,
                &message,
                note,
            ))?;
            if matches!(expected_kind, "need" | "task" | "want") {
                // A reply is a new record, so the letter can be stored under
                // the seal it reports.
                self.emit_absorbed_letter(&mut storage, &identity, &message, note)?;
            }
        }
        drop(storage);
        super::archive::export_absorbed(self, &message.message_id, expected_kind)?;
        self.storage()?.display_handle(&message.message_id)
    }

    /// The `absorbed` letter an absorb by a role that may close the item
    /// writes: a reply on the item naming the state the sealed row was in,
    /// so every member that holds the item seals its own rows the same
    /// way. A role that may not close the item (a hand, an auditor) seals
    /// only its own copy and writes no letter; nothing is written for a
    /// row that is not held work either.
    fn emit_absorbed_letter(
        &self,
        storage: &mut Storage,
        identity: &str,
        message: &StoredMessageView,
        note: Option<&str>,
    ) -> Result<(), VivariumError> {
        let Some(item) = super::item_events::held_item(storage, &message.content_id)? else {
            return Ok(());
        };
        if !self.may_close(&item, identity) {
            return Ok(());
        }
        let state = match Self::folded_item_state(storage, &item)? {
            ItemEventKind::Done => "done",
            ItemEventKind::Reopened => "open",
        };
        let extras = [("X-Vivi-Event-State", state.to_string())];
        self.emit_property_event(
            storage,
            &item,
            identity,
            PropertyKind::Absorbed,
            &extras,
            note,
        )?;
        Ok(())
    }

    /// Hold back an absorb the operator ruling of 2026-10-14 refuses: a
    /// role that may close a need, task, or want absorbs it only when its
    /// folded state is `done` and its newest held event is at least the
    /// quiet period old (`[absorb]` `quiet_hours`, 24 hours by default).
    /// A role that may not close the item, and any other kind, is untouched.
    fn require_quiet_absorb(
        &self,
        storage: &Storage,
        identity: &str,
        message: &StoredMessageView,
        handle: &str,
    ) -> Result<(), VivariumError> {
        let Some(item) = super::item_events::held_item(storage, &message.content_id)? else {
            return Ok(());
        };
        if !self.may_close(&item, identity) {
            return Ok(());
        }
        if Self::folded_item_state(storage, &item)? != ItemEventKind::Done {
            return Err(VivariumError::Message(format!(
                "{handle} is open; close it before absorbing"
            )));
        }
        let hours = AbsorbConfig::quiet_hours_of(self.config.absorb.as_ref());
        let quiet = i64::from(hours).saturating_mul(HOUR_MICROS);
        if quiet == 0 {
            return Ok(());
        }
        let Some(newest) = newest_item_event_at(storage, &item)? else {
            return Ok(());
        };
        let age = chrono::Utc::now().timestamp_micros().saturating_sub(newest);
        let left = quiet.saturating_sub(age);
        if left <= 0 {
            return Ok(());
        }
        Err(VivariumError::Message(format!(
            "{handle} is done but not quiet yet: {} left (newest event {}; quiet period {hours}h)",
            quiet_left_text(left),
            rfc3339_secs(newest),
        )))
    }

    /// The item's folded state: what the newest counted state event says
    /// (`done` or `reopened`); with no event letter, `done` when any live
    /// row of the item sits in the `done` folder, else open.
    fn folded_item_state(
        storage: &Storage,
        item: &HeldItem,
    ) -> Result<ItemEventKind, VivariumError> {
        let folded = Self::item_head(storage, item)?.unwrap_or_else(|| {
            if item.rows.iter().any(|row| row.local_role == "done") {
                ItemEventKind::Done
            } else {
                ItemEventKind::Reopened
            }
        });
        Ok(folded)
    }

    /// Move a memo to the trash. Only a memo goes: a need, task, want, or
    /// mail is work or correspondence with its own lifecycle, and trashing it
    /// here would silently drop it.
    ///
    /// # Errors
    /// Returns an error if the identity or message cannot be resolved, if the
    /// record is not a memo, or if a storage operation fails.
    pub fn delete_memo(&self, identity: &str, handle: &str) -> Result<String, VivariumError> {
        let (identity, message) = self.resolve_owned_message(identity, handle)?;
        let kind = self.source_kind(&self.storage()?, &message)?;
        if kind != "memo" {
            return Err(VivariumError::Message(format!(
                "{handle} is {kind}, not memo; memo delete only removes memos"
            )));
        }
        self.move_item(&identity, handle, "trash", None, "memo delete", None)
    }

    /// Absorb an owned mail record. After absorb, the record cannot be changed.
    ///
    /// # Errors
    /// Returns an error if the identity or message cannot be resolved, if the
    /// record is not mail, or if a storage operation fails.
    pub fn absorb_mail(
        &self,
        identity: &str,
        handle: &str,
        note: Option<&str>,
    ) -> Result<String, VivariumError> {
        self.absorb(identity, handle, note, "mail")
    }

    /// Create a task from a source message (currently only wants).
    ///
    /// # Errors
    /// Returns an error if the source message is not a want, if sending the
    /// task message fails, or if a storage operation fails.
    pub fn task_from_source(
        &self,
        request: SourceTaskRequest,
    ) -> Result<SourceTaskResult, VivariumError> {
        let (actor, source) = self.resolve_owned_message(&request.actor, &request.source_handle)?;
        let source_kind = self.source_kind(&self.storage()?, &source)?;
        if source_kind != "want" {
            return Err(VivariumError::Message(format!(
                "task from currently supports wants only; {} is {source_kind}",
                request.source_handle
            )));
        }
        let result = self.send(SendRequest {
            from: actor.clone(),
            to: request.to,
            cc: request.cc,
            bcc: Vec::new(),
            subject: request.subject,
            body: request.body,
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: Some(request.source_handle),
            depends_on: Vec::new(),
        })?;
        self.record_tasked_source(&actor, &source, &result)?;
        Ok(SourceTaskResult {
            source_handle: self.storage()?.display_handle(&source.message_id)?,
            source_kind,
            delivered: result.delivered,
            sent: result.sent,
            remote: result.remote,
        })
    }

    /// Set metadata fields (priority, rank, repo, lane, etc.) on a want
    /// message.
    ///
    /// # Errors
    /// Returns an error if the identity or message cannot be resolved, if the
    /// message is not a want, or if a storage operation fails.
    pub fn set_want_metadata(
        &self,
        identity: &str,
        handle: &str,
        update: WantMetadataUpdate,
    ) -> Result<String, VivariumError> {
        let (identity, want) = self.resolve_owned_message(identity, handle)?;
        if self.source_kind(&self.storage()?, &want)? != "want" {
            return Err(VivariumError::Message(format!("{handle} is not a want")));
        }
        if want.absorbed_at.is_some() {
            return Err(VivariumError::Message(format!(
                "{handle} is absorbed and can no longer be changed"
            )));
        }
        let mut metadata = BTreeMap::new();
        metadata.insert("priority".into(), update.priority);
        insert_optional(
            &mut metadata,
            "rank",
            update.rank.map(|rank| rank.to_string()),
        );
        insert_optional(&mut metadata, "repo", update.repo);
        insert_optional(&mut metadata, "lane", update.lane);
        insert_optional(&mut metadata, "blocks_claim", update.blocks_claim);
        insert_optional(&mut metadata, "reason", update.reason);
        let storage = self.storage()?;
        storage.set_item_metadata(&want.message_id, &metadata)?;
        storage.append_mailspace_event(&MailspaceEventInput {
            command: "want set-priority".into(),
            event_type: "metadata_updated".into(),
            actor_identity: Some(identity),
            account: want.account.clone(),
            message_id: want.message_id.clone(),
            content_id: want.content_id.clone(),
            from_role: Some(want.local_role.clone()),
            to_role: Some(want.local_role),
            from_identity: None,
            to_identity: Some(want.account),
            subject: want.subject,
            note: Some(metadata_note(&metadata)),
        })?;
        storage.display_handle(&want.message_id)
    }

    /// List wants with their metadata, filtered by roles and options.
    ///
    /// # Errors
    /// Returns an error if the identity cannot be resolved or a storage
    /// operation fails.
    #[allow(clippy::needless_pass_by_value)]
    pub fn list_wants_with_metadata(
        &self,
        identity: Option<&str>,
        roles: &[&str],
        options: WantListOptions,
    ) -> Result<Vec<WantListRecord>, VivariumError> {
        let storage = self.storage()?;
        let wants = wants_for_roles(self, identity, roles)?;
        let ids = wants
            .iter()
            .map(|want| want.message_id.clone())
            .collect::<Vec<_>>();
        let events = storage.list_mailspace_events_for_messages(&ids)?;
        let mut records = Vec::new();
        for want in wants {
            let metadata = storage.item_metadata(&want.message_id)?;
            if !metadata_matches(&metadata, &options) {
                continue;
            }
            let marks = super::marks::folded_marks(
                events.get(&want.message_id).map_or(&[][..], Vec::as_slice),
            );
            records.push(want_record(want, metadata, marks));
        }
        sort_wants(&mut records, &options.sort);
        Ok(records)
    }

    /// Run a full intake cycle: collect new events, unabsorbed mail, open needs
    /// and wants.
    ///
    /// # Errors
    /// Returns an error if the identity cannot be resolved, dump queries fail,
    /// or a storage operation fails.
    pub fn cycle_intake(
        &self,
        identity: &str,
        cursor_file: Option<&Path>,
        write_cursor: bool,
    ) -> Result<CycleIntake, VivariumError> {
        let cursor = read_cursor(cursor_file)?;
        let storage = self.storage()?;
        let events = storage.list_mailspace_events_after(cursor)?;
        let next_cursor = events.last().map_or(cursor, |event| event.event_id);
        let completed_tasks = events
            .into_iter()
            .filter(|event| event.command == "task done")
            .collect();
        let unabsorbed_mail = self.unabsorbed_mail(identity)?;
        let open_needs = self.dump_tasks(super::TaskDumpRequest {
            status: super::TaskDumpStatus::Open,
            open_role: "needs".into(),
            kind: "need".into(),
            include_outbound: false,
            filters: super::DumpFilters {
                for_identity: Some(identity.into()),
                ..Default::default()
            },
        })?;
        let open_wants = self.list_wants_with_metadata(
            Some(identity),
            &["wants"],
            WantListOptions {
                sort: "priority,rank,created".into(),
                ..Default::default()
            },
        )?;
        if write_cursor {
            write_cursor_file(cursor_file, next_cursor)?;
        }
        Ok(CycleIntake {
            cursor,
            next_cursor,
            unabsorbed_mail,
            completed_tasks,
            open_needs,
            open_wants,
        })
    }

    fn unabsorbed_mail(
        &self,
        identity: &str,
    ) -> Result<Vec<super::dump::DumpRecord>, VivariumError> {
        let mut records = self.dump_mail(super::MailDumpRequest {
            folder: "inbox".into(),
            kind: None,
            filters: super::DumpFilters {
                for_identity: Some(identity.into()),
                absorb_status: MailAbsorbFilter::Unabsorbed,
                ..Default::default()
            },
        })?;
        records.truncate(50);
        Ok(records)
    }

    pub(super) fn resolve_owned_message(
        &self,
        identity: &str,
        handle: &str,
    ) -> Result<(String, StoredMessageView), VivariumError> {
        let identity = self.resolve_identity(identity)?;
        let names = self.identity_names(&identity);
        let storage = self.storage()?;
        let resolved = storage.resolve_message_token(handle)?;
        let Some(message) = storage.message_by_id(&resolved)? else {
            return Err(VivariumError::Message(format!(
                "message not found: {handle}"
            )));
        };
        if !names.contains(&message.account) {
            return Err(self.cross_identity_error(&storage, handle));
        }
        Ok((identity, message))
    }

    /// Effective kind of a stored message. Takes the caller's storage handle so
    /// a batch shares one connection and one handle map.
    pub(super) fn source_kind(
        &self,
        storage: &crate::storage::Storage,
        message: &StoredMessageView,
    ) -> Result<String, VivariumError> {
        let data = storage.read_message(&message.message_id)?;
        let events = storage.list_mailspace_events(&message.message_id)?;
        Ok(effective_kind(&message.local_role, &data, &events).unwrap_or_else(|| "mail".into()))
    }

    fn record_tasked_source(
        &self,
        actor: &str,
        source: &StoredMessageView,
        result: &DeliveryResult,
    ) -> Result<(), VivariumError> {
        let storage = self.storage()?;
        let source_handle = storage.display_handle(&source.message_id)?;
        // With no local delivery the sent copy is the task here.
        let handles: Vec<&str> = if result.delivered.is_empty() {
            vec![result.sent.as_str()]
        } else {
            result.delivered.iter().map(|d| d.handle.as_str()).collect()
        };
        let task_handles = handles.join(",");
        for handle in handles {
            let task_id = storage.resolve_message_token(handle)?;
            let mut metadata = BTreeMap::new();
            metadata.insert("source_handle".into(), source_handle.clone());
            metadata.insert("source_content_id".into(), source.content_id.clone());
            metadata.insert("source_kind".into(), "want".into());
            storage.set_item_metadata(&task_id, &metadata)?;
        }
        storage.append_mailspace_event(&MailspaceEventInput {
            command: "task from".into(),
            event_type: "tasked".into(),
            actor_identity: Some(actor.into()),
            account: source.account.clone(),
            message_id: source.message_id.clone(),
            content_id: source.content_id.clone(),
            from_role: Some(source.local_role.clone()),
            to_role: Some(source.local_role.clone()),
            from_identity: Some(actor.into()),
            to_identity: Some(source.account.clone()),
            subject: source.subject.clone(),
            note: Some(format!("active_tasks={task_handles}; sent={}", result.sent)),
        })?;
        Ok(())
    }
}

fn insert_optional(map: &mut BTreeMap<String, String>, key: &str, value: Option<String>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        map.insert(key.into(), value);
    }
}

/// The instant of the newest event of any kind held for `item` — a state
/// event or an `absorbed`, `marked`, `unmarked` or `bound` letter a member
/// wrote and the clock counts — or, for an item closed with no event
/// letter, the instant of its `done` move in the mailspace event log.
fn newest_item_event_at(storage: &Storage, item: &HeldItem) -> Result<Option<i64>, VivariumError> {
    let now = chrono::Utc::now().timestamp_micros();
    let mut newest: Option<i64> = None;
    for child in storage.reply_child_contents(&item.content_id)? {
        let Some(view) = storage.message_by_content_id(&child)? else {
            continue;
        };
        let Ok(bytes) = storage.read_listed_blob(&view.blob_relpath) else {
            continue;
        };
        let letter = parse_event(&bytes)
            .map(|event| (event.of, event.from_domain, event.at))
            .or_else(|| {
                parse_property_event(&bytes).map(|event| (event.of, event.from_domain, event.at))
            });
        let Some((of, from_domain, at)) = letter else {
            continue;
        };
        if of != item.rfc_id
            || prefix_of_domain(&from_domain).is_none()
            || !dated_within_clock(at, now)
        {
            continue;
        }
        newest = Some(newest.map_or(at, |seen| seen.max(at)));
    }
    if newest.is_none() {
        return newest_done_move_at(storage, item);
    }
    Ok(newest)
}

/// The instant of the newest `done` move in the mailspace event log for
/// the item's rows: the quiet floor of an item closed with no event
/// letter.
fn newest_done_move_at(storage: &Storage, item: &HeldItem) -> Result<Option<i64>, VivariumError> {
    let mut newest: Option<i64> = None;
    for row in &item.rows {
        for event in storage.list_mailspace_events(&row.message_id)? {
            if event.to_role.as_deref() != Some("done") || !event.command.ends_with(" done") {
                continue;
            }
            if let Some(at) = parse_rfc3339_micros(&event.occurred_at) {
                newest = Some(newest.map_or(at, |seen| seen.max(at)));
            }
        }
    }
    Ok(newest)
}

/// An RFC 3339 instant in Unix microseconds.
fn parse_rfc3339_micros(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|time| time.timestamp_micros())
}

/// The wait left, as `3h 12m`: whole minutes, rounded up.
fn quiet_left_text(left_micros: i64) -> String {
    let minutes = left_micros.saturating_add(59_999_999) / 60_000_000;
    format!("{}h {}m", minutes / 60, minutes % 60)
}

/// An instant in Unix microseconds as RFC 3339 UTC with a `Z`.
fn rfc3339_secs(at: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_micros(at).map_or_else(
        || at.to_string(),
        |time| time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}

fn wants_for_roles(
    mailspace: &Mailspace,
    identity: Option<&str>,
    roles: &[&str],
) -> Result<Vec<StoredMessageView>, VivariumError> {
    let mut wants = Vec::new();
    for role in roles {
        let listed = if *role == "sent" {
            mailspace.list_outbound_work(identity, "want")?
        } else {
            mailspace.list_kind(identity, role, "want")?
        };
        for want in listed {
            let open_role = matches!(*role, "wants" | "sent");
            if want.absorbed_at.is_some() && open_role && !roles.contains(&"done") {
                continue;
            }
            wants.push(want);
        }
    }
    Ok(wants)
}

fn want_record(
    want: StoredMessageView,
    metadata: BTreeMap<String, String>,
    marks: Vec<String>,
) -> WantListRecord {
    WantListRecord {
        status: if want.local_role == "done" {
            "done"
        } else {
            "open"
        }
        .into(),
        handle: want.handle,
        kind: "want".into(),
        role: want.local_role,
        date: want.date,
        from: want.from_addr,
        to: want.to_addr,
        subject: want.subject,
        metadata,
        marks,
    }
}

fn metadata_note(metadata: &BTreeMap<String, String>) -> String {
    metadata
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn metadata_matches(metadata: &BTreeMap<String, String>, options: &WantListOptions) -> bool {
    options
        .repo
        .as_ref()
        .is_none_or(|repo| metadata.get("repo") == Some(repo))
        && options
            .lane
            .as_ref()
            .is_none_or(|lane| metadata.get("lane") == Some(lane))
}

fn sort_wants(records: &mut [WantListRecord], sort: &str) {
    let fields = sort.split(',').map(str::trim).collect::<Vec<_>>();
    records.sort_by(|left, right| {
        for field in &fields {
            let ordering = match *field {
                "priority" => left
                    .metadata
                    .get("priority")
                    .cmp(&right.metadata.get("priority")),
                "rank" => rank(left).cmp(&rank(right)),
                _ => right.date.cmp(&left.date),
            };
            if !ordering.is_eq() {
                return ordering;
            }
        }
        right.date.cmp(&left.date)
    });
}

fn absorb_event(
    kind: &str,
    identity: &str,
    message: &StoredMessageView,
    note: Option<&str>,
) -> MailspaceEventInput {
    MailspaceEventInput {
        command: format!("{kind} absorb"),
        event_type: "absorbed".into(),
        actor_identity: Some(identity.into()),
        account: message.account.clone(),
        message_id: message.message_id.clone(),
        content_id: message.content_id.clone(),
        from_role: Some(message.local_role.clone()),
        to_role: Some(message.local_role.clone()),
        from_identity: None,
        to_identity: Some(message.account.clone()),
        subject: message.subject.clone(),
        note: note.map(str::to_string),
    }
}

fn rank(record: &WantListRecord) -> i64 {
    record
        .metadata
        .get("rank")
        .and_then(|rank| rank.parse().ok())
        .unwrap_or(i64::MAX)
}

fn read_cursor(path: Option<&Path>) -> Result<i64, VivariumError> {
    let Some(path) = path else {
        return Ok(0);
    };
    if !path.exists() {
        return Ok(0);
    }
    std::fs::read_to_string(path)?
        .trim()
        .parse()
        .map_err(|_| VivariumError::Config(format!("invalid cursor file {}", path.display())))
}

fn write_cursor_file(path: Option<&Path>, cursor: i64) -> Result<(), VivariumError> {
    let Some(path) = path else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{cursor}\n"))?;
    Ok(())
}

#[cfg(test)]
#[path = "absorb_quiet_test.rs"]
mod absorb_quiet_tests;
