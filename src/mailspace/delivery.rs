use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::Utc;

use super::event_log::{log_move_event, log_send_events};
use super::kind::matches_kind;
use super::reply::{add_reply_headers, resolve_reply_parent, roots_only};
use super::{
    DeliveredMessage, DeliveryResult, Mailspace, MoveOutcome, RawDelivery, Recipient, SendRequest,
    canonical_local_role,
};
use crate::error::VivariumError;
use crate::letter::{FORMAT_HEADER, FORMAT_VERSION, Letter};
use crate::message::{ComposeDraft, build_compose_draft, validate_message_headers};
use crate::storage::{
    MailspaceEventInput, MailspaceMoveWithReply, MessageIngestRequest, Storage, StoredMessageView,
};
use crate::sync::{MAX_LETTER_BYTES, SyncSettings};

mod compose;
mod ingest;
mod retention;

pub(in crate::mailspace) use ingest::DeliveredMessageId;
use ingest::{
    collect_addresses, delivered_with_handles, ingest_for_recipients, ingest_sent_copy,
    raw_delivery_requests, warn_duplicate_work_subjects,
};

impl Mailspace {
    /// Send a message to identities in the mailspace and to remote addresses.
    /// `request` specifies the sender, recipients, subject, body, and optional
    /// role. Sent and delivered copies are created in the store.
    ///
    /// A recipient that is a role of this mailspace gets a delivered copy. A
    /// recipient that is an address in another mailspace
    /// (`role@<prefix>.local`) gets none: its address is written in the To or
    /// Cc header, the sent copy is the local record, and `vivi sync` carries
    /// the letter. Those addresses are listed in [`DeliveryResult::remote`].
    ///
    /// # Errors
    /// Returns an error if the sender or any recipient identity cannot be
    /// resolved, if message composition fails, or if a storage operation fails.
    pub fn send(&self, request: SendRequest) -> Result<DeliveryResult, VivariumError> {
        self.send_with_extras(request, &[])
    }

    /// [`Mailspace::send`] with extra headers written on the letter, in order,
    /// after the kind and dependency headers. Used by a caller that must mark
    /// the letter with where it came from (`task from` names its parent).
    ///
    /// # Errors
    /// Returns the errors [`Mailspace::send`] returns.
    #[allow(clippy::needless_pass_by_value)]
    pub(super) fn send_with_extras(
        &self,
        mut request: SendRequest,
        extras: &[(String, String)],
    ) -> Result<DeliveryResult, VivariumError> {
        if !request.bcc.is_empty() {
            return Err(VivariumError::Message(
                "Bcc is not supported for local mailspace delivery in v1".into(),
            ));
        }
        let work_letter = compose::work_letter(
            self,
            request.kind.as_deref(),
            &request.body,
            &request.depends_on,
        )?;
        let depends_on = work_letter
            .as_ref()
            .map_or_else(|| request.depends_on.clone(), |w| w.depends_on.clone());
        if let Some(work) = &work_letter {
            request.body.clone_from(&work.body);
        }
        let work_kind = work_send_kind(&request.role, request.kind.as_deref());
        if work_kind.is_some() && !depends_on.is_empty() {
            self.backlog_validate_deps(&depends_on)?;
        }
        let from = self.resolve_identity(&request.from)?;
        let (recipients, remote) = self.split_recipients(&request.to, &request.cc)?;
        if recipients.is_empty() && remote.is_empty() {
            return Err(VivariumError::Message(
                "delivery needs at least one To or Cc recipient".into(),
            ));
        }
        let storage = self.storage()?;
        let reply_parent = request
            .reply_to
            .as_deref()
            .map(|handle| resolve_reply_parent(&storage, handle))
            .transpose()?;
        let eml = self.compose_message(
            &from,
            &request,
            reply_parent.as_ref().map(|(_, data)| data.as_slice()),
            extras,
            work_letter.is_some(),
        )?;
        self.check_letter_size(eml.len())?;
        if work_kind.is_some() {
            let from_addr = self.address_for(&from);
            warn_duplicate_work_subjects(
                &storage,
                &request.role,
                &request.subject,
                &from_addr,
                &recipients,
            );
        }
        let seed = Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let delivered_ids = ingest_for_recipients(&storage, recipients, &request.role, &eml, seed)?;
        let sent = ingest_sent_copy(&storage, &from, &eml, seed)?;
        if let Some((parent, _)) = &reply_parent {
            storage.link_mailspace_content(&sent.content_id, &parent.content_id, "captured")?;
        }
        log_send_events(
            &storage,
            &from,
            &request,
            &delivered_ids,
            &sent,
            reply_parent.as_ref().map(|(parent, _)| parent),
        )?;
        let delivered = delivered_with_handles(&storage, delivered_ids)?;
        let sent_handle = storage.display_handle(&sent.message_id)?;
        if let Some(kind) = work_kind {
            let letter = work_letter.as_ref().map(|w| &w.letter);
            self.attach_sent_work(
                kind,
                &request,
                &depends_on,
                &delivered,
                &sent_handle,
                letter,
            )?;
        }
        Ok(DeliveryResult {
            delivered,
            sent: sent_handle,
            remote: remote.into_iter().collect(),
        })
    }

    /// Mint the backlog node for each delivered copy of a work item. With no
    /// local delivery the sent copy is the only row here, so it is the item.
    fn attach_sent_work(
        &self,
        kind: &str,
        request: &SendRequest,
        depends_on: &[String],
        delivered: &[DeliveredMessage],
        sent_handle: &str,
        letter: Option<&Letter>,
    ) -> Result<(), VivariumError> {
        let handles: Vec<&str> = if delivered.is_empty() {
            vec![sent_handle]
        } else {
            delivered.iter().map(|m| m.handle.as_str()).collect()
        };
        for handle in handles {
            self.backlog_attach(kind, handle, &request.subject, depends_on)?;
            if let Some(letter) = letter {
                let storage = self.storage()?;
                let row = storage.resolve_message_token(handle)?;
                super::serves::index_front(&storage, &row, letter)?;
            }
        }
        Ok(())
    }

    /// Save a self-addressed memo into the identity's `memos` folder.
    /// Unlike `send`, this creates no sent copy and no recipient delivery —
    /// it is a single-actor persistence operation.
    ///
    /// # Errors
    /// Returns an error if the identity cannot be resolved, a tag is empty or
    /// contains whitespace, message composition fails, or the storage
    /// operation fails.
    pub fn save_memo(
        &self,
        identity: &str,
        subject: &str,
        body: &str,
        tags: &[String],
    ) -> Result<String, VivariumError> {
        let tags = normalized_memo_tags(tags)?;
        let identity = self.resolve_identity(identity)?;
        let address = self.address_for(&identity);
        let mut eml = build_compose_draft(&ComposeDraft {
            from: address.clone(),
            to: vec![address],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.to_string(),
            body: body.to_string(),
            html_body: None,
        })?;
        eml = add_header(&eml, "X-Vivi-Kind", "memo")?;
        for tag in &tags {
            eml = add_header(&eml, "X-Vivi-Tag", tag)?;
        }
        let seed = Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let storage = self.storage()?;
        let stored = storage.ingest_message(
            &MessageIngestRequest {
                account: identity.clone(),
                local_role: "memos".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("memo-save\0{seed}\0{identity}"),
                remote: None,
            },
            eml.as_bytes(),
        )?;
        let handle = storage.display_handle(&stored.message_id)?;
        drop(storage);
        retention::prune_retained_memos(self, &identity, &tags)?;
        Ok(handle)
    }

    fn compose_message(
        &self,
        from: &str,
        request: &SendRequest,
        parent: Option<&[u8]>,
        extras: &[(String, String)],
        work_letter: bool,
    ) -> Result<String, VivariumError> {
        let to = self.addresses_for(&request.to)?;
        let cc = self.addresses_for(&request.cc)?;
        let mut eml = build_compose_draft(&ComposeDraft {
            from: self.address_for(from),
            to,
            cc,
            bcc: Vec::new(),
            subject: request.subject.clone(),
            body: request.body.clone(),
            html_body: None,
        })?;
        if let Some(kind) = &request.kind {
            eml = add_header(&eml, "X-Vivi-Kind", kind)?;
        }
        if work_letter {
            eml = add_header(&eml, FORMAT_HEADER, FORMAT_VERSION)?;
        } else {
            for dependency in &request.depends_on {
                eml = add_header(&eml, "X-Vivi-Depends-On", dependency)?;
            }
        }
        for (name, value) in extras {
            eml = add_header(&eml, name, value)?;
        }
        if let Some(parent) = parent {
            eml = add_reply_headers(&eml, parent)?;
        }
        Ok(eml)
    }

    pub(super) fn addresses_for(&self, values: &[String]) -> Result<Vec<String>, VivariumError> {
        values
            .iter()
            .map(|value| self.header_address(value))
            .collect()
    }

    /// Whether `[sync]` is complete enough for `vivi sync` to run here.
    #[must_use]
    pub fn sync_configured(&self) -> bool {
        SyncSettings::resolve(self).is_ok()
    }

    /// Refuse a letter the project server would never take. Only a mailspace
    /// with `[sync]` configured has such a cap; without sync nothing leaves
    /// the checkout and any size is stored.
    ///
    /// # Errors
    /// Returns an error naming the size and the cap when `len` is over it.
    pub(super) fn check_letter_size(&self, len: usize) -> Result<(), VivariumError> {
        let size = u64::try_from(len).unwrap_or(u64::MAX);
        if size <= MAX_LETTER_BYTES || !self.sync_configured() {
            return Ok(());
        }
        Err(VivariumError::Message(format!(
            "letter is {size} bytes, over the {MAX_LETTER_BYTES}-byte (64 KiB) cap the project \
             server accepts; shorten it or split it into several letters (nothing was stored)"
        )))
    }

    /// The operator line for recipients in other mailspaces: the queued
    /// confirmation when sync is configured, else a note that the letter
    /// stays in this checkout. `None` for no remote recipients.
    #[must_use]
    pub fn remote_line(&self, remote: &[String]) -> Option<String> {
        if remote.is_empty() {
            return None;
        }
        let list = remote.join(", ");
        Some(if self.sync_configured() {
            format!("queued for sync: {list}")
        } else {
            format!(
                "note: no [sync] is configured in this mailspace; the letter to {list} \
                 will not leave this checkout"
            )
        })
    }

    /// Print [`Mailspace::remote_line`]: the queued confirmation on stdout,
    /// the no-sync note on stderr.
    pub fn announce_remote(&self, remote: &[String]) {
        match self.remote_line(remote) {
            Some(line) if self.sync_configured() => println!("{line}"),
            Some(line) => eprintln!("{line}"),
            None => {}
        }
    }

    /// Deliver a raw `.eml` message into the mailspace.
    ///
    /// Each To and Cc address is classified as `send` does: a role of this
    /// mailspace gets a delivered copy, an address in another mailspace
    /// (`role@<prefix>.local`) is listed in [`RawDelivery::remote`] and
    /// reaches its holder when `vivi sync` pushes the letter, and anything
    /// else is an error. A letter with no local recipient is kept as the
    /// sender's own copy (`sent`), so its From must be a role of this
    /// mailspace. The bytes are stored as given.
    ///
    /// # Errors
    /// Returns an error if the message cannot be parsed, a recipient is
    /// neither a local role nor a remote address, the letter is over the sync
    /// cap, or the storage operation fails.
    pub fn deliver_raw(&self, data: &[u8], folder: &str) -> Result<RawDelivery, VivariumError> {
        validate_message_headers(data)?;
        self.check_letter_size(data.len())?;
        let parsed = mail_parser::MessageParser::default()
            .parse(data)
            .ok_or_else(|| VivariumError::Parse("failed to parse message".into()))?;
        if parsed.bcc().is_some_and(|a| a.first().is_some()) {
            return Err(VivariumError::Message(
                "Bcc is not supported for local mailspace delivery in v1".into(),
            ));
        }
        let mut addresses = BTreeSet::new();
        collect_addresses(parsed.to(), &mut addresses);
        collect_addresses(parsed.cc(), &mut addresses);
        let addresses: Vec<String> = addresses.into_iter().collect();
        let (local, remote) = self.split_recipients(&addresses, &[])?;
        let mut role = canonical_local_role(folder)?;
        let mut recipients: Vec<String> = local.into_iter().collect();
        let held = recipients.is_empty();
        if held {
            if remote.is_empty() {
                return Err(VivariumError::Message(
                    "no recipient address resolves to a known local identity".into(),
                ));
            }
            recipients.push(self.raw_sender(&parsed)?);
            role = "sent".into();
        }
        let requests = raw_delivery_requests(&recipients, &role, data.len());
        let subject = parsed.subject().unwrap_or_default().to_string();
        let storage = self.storage()?;
        let stored = storage.deliver_raw_batch(
            &requests,
            data,
            "mail deliver",
            "delivered",
            &role,
            &subject,
        )?;
        let kind = super::kind::raw_work_kind(data, &role);
        self.backlog_attach_raw_delivery(&stored, kind, &subject, data)?;
        let mut delivered = stored
            .into_iter()
            .zip(requests.iter())
            .map(|(msg, req)| {
                Ok(DeliveredMessage {
                    identity: req.account.clone(),
                    handle: storage.display_handle(&msg.message_id)?,
                })
            })
            .collect::<Result<Vec<_>, VivariumError>>()?;
        let sent = held.then(|| delivered.remove(0).handle);
        Ok(RawDelivery {
            delivered,
            sent,
            remote: remote.into_iter().collect(),
        })
    }

    /// The local role a letter addressed only to other mailspaces is kept
    /// under: its From, which must be a role of this mailspace.
    fn raw_sender(&self, parsed: &mail_parser::Message<'_>) -> Result<String, VivariumError> {
        let mut from = BTreeSet::new();
        collect_addresses(parsed.from(), &mut from);
        let Some(from) = from.into_iter().next() else {
            return Err(VivariumError::Message(
                "a letter addressed only to other mailspaces needs a From that is a role of \
                 this mailspace; the letter has no From"
                    .into(),
            ));
        };
        match self.classify_recipient(&from) {
            Ok(Recipient::Local(identity)) => Ok(identity),
            _ => Err(VivariumError::Message(format!(
                "a letter addressed only to other mailspaces needs a From that is a role of \
                 this mailspace; '{from}' is not one"
            ))),
        }
    }

    /// List messages for a given identity and canonical role.
    ///
    /// # Errors
    /// Returns an error if the identity cannot be resolved or the storage
    /// operation fails.
    pub fn list(
        &self,
        identity: &str,
        role: &str,
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        let identity = self.resolve_identity(identity)?;
        let names = self.identity_names(&identity);
        let role = canonical_local_role(role)?;
        let storage = self.storage()?;
        Ok(storage
            .list_messages_by_role(&role)?
            .into_iter()
            .filter(|message| names.contains(&message.account))
            .collect())
    }

    /// List messages for a given identity, role, and kind (e.g. `task`, `need`).
    ///
    /// # Errors
    /// Returns an error if the identity cannot be resolved, the role is invalid,
    /// or the storage operation fails.
    pub fn list_kind(
        &self,
        identity: Option<&str>,
        role: &str,
        kind: &str,
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        self.list_kind_inner(identity, role, kind, true)
    }

    /// List every message for an identity, role, and kind, including replies
    /// to another work item. Use when the reply itself is a separate work
    /// item; ordinary work boards should use [`Mailspace::list_kind`].
    ///
    /// # Errors
    /// Returns an error if the identity or role is invalid, or storage fails.
    pub fn list_kind_all(
        &self,
        identity: Option<&str>,
        role: &str,
        kind: &str,
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        self.list_kind_inner(identity, role, kind, false)
    }

    fn list_kind_inner(
        &self,
        identity: Option<&str>,
        role: &str,
        kind: &str,
        collapse_replies: bool,
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        let role = canonical_local_role(role)?;
        let storage = self.storage()?;
        let role_messages = if let Some(identity) = identity {
            let identity = self.resolve_identity(identity)?;
            let names = sorted_identity_names(self.identity_names(&identity));
            storage.list_messages_by_account_roles_scoped(&names, std::slice::from_ref(&role))?
        } else {
            storage.list_messages_by_role(&role)?
        };
        let mut messages = Vec::new();
        if role_implies_kind(&role, kind) {
            messages = role_messages;
        } else {
            for message in role_messages {
                let data = storage.read_message(&message.message_id)?;
                let events = storage.list_mailspace_events(&message.message_id)?;
                if matches_kind(&message.local_role, &data, &events, kind) {
                    messages.push(message);
                }
            }
        }
        if collapse_replies {
            roots_only(&storage, messages)
        } else {
            Ok(messages)
        }
    }

    /// The sender's own open copies of `kind` work addressed to another
    /// mailspace: rows still in `sent` with a remote To or Cc. These are the
    /// author's outbound work in flight, which the receiving mailspace owns.
    ///
    /// # Errors
    /// Returns an error if the identity cannot be resolved or the storage
    /// operation fails.
    pub fn list_outbound_work(
        &self,
        identity: Option<&str>,
        kind: &str,
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        let storage = self.storage()?;
        let sent = [String::from("sent")];
        let rows = if let Some(identity) = identity {
            let identity = self.resolve_identity(identity)?;
            let names = sorted_identity_names(self.identity_names(&identity));
            storage.list_messages_by_account_roles_scoped(&names, &sent)?
        } else {
            storage.list_messages_by_role("sent")?
        };
        let mut messages = Vec::new();
        for row in rows {
            if self.remote_recipients(&row).is_empty() {
                continue;
            }
            let data = storage.read_message(&row.message_id)?;
            let events = storage.list_mailspace_events(&row.message_id)?;
            if matches_kind(&row.local_role, &data, &events, kind) {
                messages.push(row);
            }
        }
        roots_only(&storage, messages)
    }

    /// Search memos for an identity by query.
    ///
    /// Matches subject and body (unless `subject_only` is true). Search is
    /// case-insensitive. Only memos whose subject does not already match are
    /// parsed for body text, so the common case is cheap.
    ///
    /// # Errors
    /// Returns an error if the identity cannot be resolved, a storage
    /// operation fails, or a memo blob cannot be parsed.
    pub fn search_memos(
        &self,
        identity: &str,
        query: &str,
        subject_only: bool,
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        let query = query.to_lowercase();
        let memos = self.list_kind(Some(identity), "memos", "memo")?;
        if subject_only {
            return Ok(memos
                .into_iter()
                .filter(|memo| memo.subject.to_lowercase().contains(&query))
                .collect());
        }
        let storage = self.storage()?;
        let mut matched = Vec::new();
        for memo in memos {
            if memo.subject.to_lowercase().contains(&query) {
                matched.push(memo);
                continue;
            }
            let data = storage.read_message(&memo.message_id)?;
            let extracted = crate::extract::extract_text(&data)?;
            if extracted.to_lowercase().contains(&query) {
                matched.push(memo);
            }
        }
        Ok(matched)
    }

    /// Extract the dependency handles recorded on a task message.
    ///
    /// Dependencies are stored in the task's `depends_on` front matter, then
    /// changed by any `dependency-added` / `dependency-removed` letters on the
    /// task: the newest letter per dependency wins over the header.
    ///
    /// # Errors
    /// Returns an error if the message cannot be read or parsed.
    pub fn task_depends_on(&self, message_id: &str) -> Result<Vec<String>, VivariumError> {
        let storage = self.storage()?;
        let data = storage.read_message(message_id)?;
        let header = super::dependencies::depends_on_refs(&data);
        self.effective_dependency_refs(&storage, message_id, &header)
    }

    /// List open tasks with their dependency handles, optionally filtered.
    ///
    /// - `blocked` keeps only tasks with at least one unmet dependency: a
    ///   dependency is met only when it is present in this mailspace and done,
    ///   so a reference that is not held yet, or can never name held work,
    ///   still blocks the task.
    /// - `blocking` keeps only tasks that depend on the given handle.
    ///
    /// # Errors
    /// Returns an error if the identity cannot be resolved or a storage
    /// operation fails.
    pub fn list_tasks_with_deps(
        &self,
        identity: &str,
        blocked: bool,
        blocking: Option<&str>,
    ) -> Result<Vec<(StoredMessageView, Vec<String>)>, VivariumError> {
        let tasks = self.list_kind(Some(identity), "tasks", "task")?;
        let mut result = Vec::new();
        for task in tasks {
            let deps = self.task_depends_on(&task.message_id)?;
            if deps.is_empty() {
                continue;
            }
            if let Some(blocking) = blocking
                && !deps.iter().any(|d| d == blocking)
            {
                continue;
            }
            if blocked {
                let mut has_unmet = false;
                for dep in &deps {
                    if self.dependency_is_unmet(dep)? {
                        has_unmet = true;
                        break;
                    }
                }
                if !has_unmet {
                    continue;
                }
            }
            result.push((task, deps));
        }
        Ok(result)
    }

    /// Move a task message to a new role (e.g. `done`, `tasks`). If `note` is
    /// provided, a reply message is also created.
    ///
    /// # Errors
    /// Returns an error if the identity or message cannot be resolved, the role
    /// is invalid, or the storage operation fails.
    #[allow(clippy::too_many_arguments)]
    pub fn move_task(
        &self,
        identity: &str,
        handle: &str,
        role: &str,
        note: Option<&str>,
        verdict: Option<&str>,
        repo: &[String],
        tip: &[String],
    ) -> Result<String, VivariumError> {
        self.move_task_outcome(identity, handle, role, note, verdict, repo, tip)
            .map(|moved| moved.handle)
    }

    /// [`Mailspace::move_task`], reporting whether the move travelled as an
    /// event letter or stayed a local report.
    ///
    /// # Errors
    /// Same as [`Mailspace::move_task`].
    #[allow(clippy::too_many_arguments)]
    pub fn move_task_outcome(
        &self,
        identity: &str,
        handle: &str,
        role: &str,
        note: Option<&str>,
        verdict: Option<&str>,
        repo: &[String],
        tip: &[String],
    ) -> Result<MoveOutcome, VivariumError> {
        if repo.len() != tip.len() {
            return Err(VivariumError::Other(
                "--repo and --tip must be provided in matching pairs".into(),
            ));
        }
        let mut metadata = BTreeMap::new();
        if let Some(verdict) = verdict {
            metadata.insert("verdict".into(), verdict.into());
        }
        for (repo, tip) in repo.iter().zip(tip.iter()) {
            metadata.insert(format!("repo:{repo}"), tip.clone());
        }
        self.move_item_outcome(
            identity,
            handle,
            role,
            note,
            move_command("task", role),
            if metadata.is_empty() {
                None
            } else {
                Some(&metadata)
            },
        )
    }

    fn move_event(
        command: &str,
        identity: &str,
        account: &str,
        before: &StoredMessageView,
        role: &str,
        note: &str,
    ) -> MailspaceEventInput {
        MailspaceEventInput {
            command: command.into(),
            event_type: "moved".into(),
            actor_identity: Some(identity.into()),
            account: account.into(),
            message_id: before.message_id.clone(),
            content_id: before.content_id.clone(),
            from_role: Some(before.local_role.clone()),
            to_role: Some(role.into()),
            from_identity: Some(identity.into()),
            to_identity: Some(identity.into()),
            subject: before.subject.clone(),
            note: Some(note.into()),
        }
    }

    /// Move a message to a new role with an optional note reply.
    ///
    /// # Errors
    /// Returns an error if the identity or message cannot be resolved, the role
    /// is invalid, note reply composition fails, or the storage operation fails.
    pub fn move_item(
        &self,
        identity: &str,
        handle: &str,
        role: &str,
        note: Option<&str>,
        command: &str,
        metadata: Option<&BTreeMap<String, String>>,
    ) -> Result<String, VivariumError> {
        self.move_item_outcome(identity, handle, role, note, command, metadata)
            .map(|moved| moved.handle)
    }

    /// [`Mailspace::move_item`], reporting whether the move travelled as an
    /// event letter or stayed a local report, plus the handles of any bound
    /// needs the move completed through its join.
    ///
    /// # Errors
    /// Same as [`Mailspace::move_item`].
    pub fn move_item_outcome(
        &self,
        identity: &str,
        handle: &str,
        role: &str,
        note: Option<&str>,
        command: &str,
        metadata: Option<&BTreeMap<String, String>>,
    ) -> Result<MoveOutcome, VivariumError> {
        Self::validate_move_metadata(metadata)?;
        let identity = self.resolve_identity(identity)?;
        let role = canonical_local_role(role)?;
        let storage = self.storage()?;
        let before = self.move_target(&storage, &identity, handle, &role, command)?;
        if before.absorbed_at.is_some() {
            return Err(VivariumError::Message(format!(
                "{handle} is absorbed and can no longer be changed"
            )));
        }
        let work_close = self.is_work_close(&storage, &before, &role)?;
        let mut joined_needs = Vec::new();
        // A role that may close the work (its author, or a mind) writes an
        // event letter, which carries the move to every mailspace holding
        // the item; any other move is local, and a close is a report.
        let extras = metadata.map(Self::verdict_headers).unwrap_or_default();
        let event = self.try_emit_for_move(
            &storage,
            &identity,
            &before,
            &role,
            note,
            command,
            &extras,
            &mut joined_needs,
        )?;
        if event.is_none() {
            self.refuse_sent_copy_reopen(handle, &before, &role)?;
            let note = report_note(note, &extras);
            self.move_row_locally(&storage, &identity, &before, &role, note, command, &extras)?;
        }
        Self::store_move_metadata(&storage, &before.message_id, metadata)?;
        let shown = storage.display_handle(&before.message_id)?;
        if event.is_none() {
            self.sync_backlog_after_move(
                &storage,
                &identity,
                &before,
                &role,
                &shown,
                &mut joined_needs,
            )?;
        }
        Ok(MoveOutcome {
            handle: shown,
            reported: event.is_none() && work_close,
            event,
            joined_needs,
        })
    }

    /// Check that close provenance arrives as one nonempty branch/commit pair.
    fn validate_move_metadata(
        metadata: Option<&BTreeMap<String, String>>,
    ) -> Result<(), VivariumError> {
        if metadata
            .is_some_and(|fields| fields.contains_key("branch") != fields.contains_key("commit"))
        {
            return Err(VivariumError::Other(
                "branch and commit close metadata must be provided together".into(),
            ));
        }
        if metadata.is_some_and(|fields| {
            [fields.get("branch"), fields.get("commit")]
                .into_iter()
                .flatten()
                .any(|value| value.trim().is_empty())
        }) {
            return Err(VivariumError::Other(
                "branch and commit close metadata cannot be empty".into(),
            ));
        }
        Ok(())
    }

    /// Persist close provenance after its move succeeds, when supplied.
    fn store_move_metadata(
        storage: &Storage,
        message_id: &str,
        metadata: Option<&BTreeMap<String, String>>,
    ) -> Result<(), VivariumError> {
        if let Some(metadata) = metadata {
            storage.set_item_metadata(message_id, metadata)?;
        }
        Ok(())
    }

    /// The row a move acts on: the one the identity holds under `handle`, or
    /// any row of the same item when the identity may close it.
    fn move_target(
        &self,
        storage: &Storage,
        identity: &str,
        handle: &str,
        role: &str,
        command: &str,
    ) -> Result<StoredMessageView, VivariumError> {
        let names = sorted_identity_names(self.identity_names(identity));
        let Ok(resolved) = storage.resolve_message_token_for_accounts(handle, &names) else {
            return self
                .row_for_item_actor(storage, identity, handle, role)?
                .ok_or_else(|| self.foreign_move_error(storage, identity, handle, role, command));
        };
        let Some(before) = storage.message_by_id(&resolved)? else {
            return Err(VivariumError::Message(format!(
                "message not found: {handle}"
            )));
        };
        if !names.contains(&before.account) {
            return Err(VivariumError::Message(format!(
                "message not found for {identity}: {handle}"
            )));
        }
        Ok(before)
    }

    /// Move the one row `before` names, with the note as a reply when given.
    fn move_row_locally(
        &self,
        storage: &Storage,
        identity: &str,
        before: &StoredMessageView,
        role: &str,
        note: Option<&str>,
        command: &str,
        extras: &[(String, String)],
    ) -> Result<(), VivariumError> {
        // Stored messages keep the account name they were ingested under
        // even after a rename, so the storage-layer mutation must target
        // that historical account rather than the current canonical name.
        let account = before.account.clone();
        let Some(note) = note else {
            storage.move_message_to_role(&account, &before.message_id, role)?;
            return log_move_event(storage, identity, role, before, command, None);
        };
        let reply = self.note_reply(storage, identity, before, note, extras)?;
        let event = Self::move_event(command, identity, &account, before, role, note);
        storage.move_message_with_reply(&MailspaceMoveWithReply {
            account: &account,
            message_id: &before.message_id,
            local_role: role,
            event: &event,
            reply_requests: &reply.requests,
            reply_data: &reply.data,
            parent_content_id: &before.content_id,
        })?;
        Ok(())
    }

    /// Error for a token that resolves globally but is not held by the
    /// acting identity: name the holder so the operator can retry with
    /// `--for <holder>`. Falls back to a plain not-found error.
    pub(super) fn cross_identity_error(&self, storage: &Storage, token: &str) -> VivariumError {
        let Ok(message_id) = storage.resolve_message_token(token) else {
            return VivariumError::Message(format!("message not found: {token}"));
        };
        let holder = match storage.message_by_id(&message_id) {
            Ok(Some(view)) => self.identity_owning_account(&view.account),
            _ => None,
        };
        match holder {
            Some(holder) => VivariumError::Message(format!(
                "message '{token}' is held by identity '{holder}'; retry with --for {holder}"
            )),
            None => VivariumError::Message(format!("message not found: {token}")),
        }
    }
}

/// A verdict-only report still sends a letter, so the owner sees the verdict.
fn report_note<'a>(note: Option<&'a str>, extras: &[(String, String)]) -> Option<&'a str> {
    note.or((!extras.is_empty()).then_some("done"))
}

fn role_implies_kind(role: &str, kind: &str) -> bool {
    matches!(
        (role, kind),
        ("tasks", "task")
            | ("needs", "need")
            | ("wants", "want")
            | ("pumps", "pump")
            | ("memos", "memo")
            | ("rules", "rule")
    )
}

/// The backlog-citizenship kind for a send, if this role+kind pair mints a
/// work-graph node.
fn work_send_kind<'a>(role: &str, kind: Option<&'a str>) -> Option<&'a str> {
    match (role, kind) {
        ("tasks", Some("task")) | ("needs", Some("need")) | ("wants", Some("want")) => kind,
        _ => None,
    }
}

fn sorted_identity_names(names: HashSet<String>) -> Vec<String> {
    let mut names = names.into_iter().collect::<Vec<_>>();
    names.sort();
    names
}

fn move_command(kind: &str, role: &str) -> &'static str {
    match (kind, role) {
        ("task", "done") => "task done",
        ("task", _) => "task reopen",
        _ => "item move",
    }
}

/// Trim memo tags and reject ones that are empty or contain whitespace, so
/// every stored tag is a single token.
fn normalized_memo_tags(tags: &[String]) -> Result<Vec<String>, VivariumError> {
    let mut normalized = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = tag.trim();
        if tag.is_empty() || tag.chars().any(char::is_whitespace) {
            return Err(VivariumError::Message(format!(
                "memo tag {tag:?} must be non-empty and contain no whitespace"
            )));
        }
        normalized.push(tag.to_string());
    }
    Ok(normalized)
}

pub(super) fn add_header(eml: &str, name: &str, value: &str) -> Result<String, VivariumError> {
    let newline = if eml.contains("\r\n") { "\r\n" } else { "\n" };
    let separator = format!("{newline}{newline}");
    let (headers, body) = eml
        .split_once(&separator)
        .ok_or_else(|| VivariumError::Message("message has no header/body separator".into()))?;
    Ok(format!(
        "{headers}{newline}{name}: {value}{separator}{body}"
    ))
}

#[cfg(test)]
#[path = "delivery_test.rs"]
mod tests;
