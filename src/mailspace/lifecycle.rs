use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use super::kind::effective_kind;
use super::{DeliveryResult, Mailspace, SendRequest};
use crate::error::VivariumError;
use crate::storage::{MailspaceEvent, MailspaceEventInput, StoredMessageView};

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
        let mut storage = storage;
        let newly = storage.absorb_message(&message.account, &message.message_id, &identity)?;
        if newly {
            storage.append_mailspace_event(&absorb_event(
                expected_kind,
                &identity,
                &message,
                note,
            ))?;
        }
        drop(storage);
        super::archive::export_absorbed(self, &message.message_id, expected_kind)?;
        self.storage()?.display_handle(&message.message_id)
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
        let task_handles = result
            .delivered
            .iter()
            .map(|delivered| delivered.handle.as_str())
            .collect::<Vec<_>>()
            .join(",");
        for delivered in &result.delivered {
            let task_id = storage.resolve_message_token(&delivered.handle)?;
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

fn wants_for_roles(
    mailspace: &Mailspace,
    identity: Option<&str>,
    roles: &[&str],
) -> Result<Vec<StoredMessageView>, VivariumError> {
    let mut wants = Vec::new();
    for role in roles {
        for want in mailspace.list_kind(identity, role, "want")? {
            if want.absorbed_at.is_some() && *role == "wants" && !roles.contains(&"done") {
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
