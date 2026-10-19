use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::Mailspace;
use crate::error::VivariumError;
use crate::storage::{MailspaceEventInput, Storage, StoredMessageView};

#[derive(Debug, Clone)]
pub struct MailspaceImportOptions {
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MailspaceImportReport {
    pub dry_run: bool,
    pub source: PathBuf,
    pub target: PathBuf,
    pub scanned_messages: usize,
    pub imported_messages: usize,
    pub deduped_messages: usize,
    pub imported_blobs: usize,
    pub deduped_blobs: usize,
    pub imported_events: usize,
    pub deduped_events: usize,
    pub imported_links: usize,
    pub deduped_links: usize,
    /// Soft conflicts: reported, and the rest of the import still applies.
    pub conflicts: Vec<String>,
    /// Source messages whose remote position another message in the target
    /// already holds. They cannot be filed, so a real import refuses whole.
    pub binding_conflicts: Vec<String>,
}

/// Binding conflicts listed in the refusal before the rest are summarized.
const LISTED_CONFLICTS: usize = 5;

impl MailspaceImportReport {
    /// The refusal for this report, if any message cannot be filed.
    #[must_use]
    pub fn binding_conflict_error(&self) -> Option<VivariumError> {
        if self.binding_conflicts.is_empty() {
            return None;
        }
        let mut lines: Vec<String> = self
            .binding_conflicts
            .iter()
            .take(LISTED_CONFLICTS)
            .map(|conflict| format!("  {conflict}"))
            .collect();
        if self.binding_conflicts.len() > LISTED_CONFLICTS {
            lines.push(format!(
                "  ... and {} more",
                self.binding_conflicts.len() - LISTED_CONFLICTS
            ));
        }
        let outcome = if self.dry_run {
            "a real import would be refused"
        } else {
            "nothing was imported"
        };
        Some(VivariumError::Message(format!(
            "import conflict: {} message(s) are bound to a project item that a different \
             message in this mailspace already holds, so the two mailspaces cannot be merged \
             as they are; {outcome}\n{}",
            self.binding_conflicts.len(),
            lines.join("\n")
        )))
    }
}

/// Import messages, events, and links from a source mailspace into the
/// target mailspace.
///
/// # Errors
/// Returns an error if the source path is not a valid mailspace, if source
/// and target are the same path, or if any storage operation fails.
#[allow(clippy::needless_pass_by_value)]
pub fn import_mailspace(
    target: &Mailspace,
    source: &Path,
    options: MailspaceImportOptions,
) -> Result<MailspaceImportReport, VivariumError> {
    let source_dir = source_mailspace_dir(source)?;
    let target_dir = target.dir.clone();
    if same_path(&source_dir, &target_dir) {
        return Err(VivariumError::Message(
            "source and target mailspaces are the same path".into(),
        ));
    }

    let source_storage = Storage::open_mailspace(&source_dir)?;
    let target_storage = target.storage()?;
    let mut report = MailspaceImportReport {
        dry_run: options.dry_run,
        source: source_dir,
        target: target_dir,
        ..Default::default()
    };
    if options.dry_run {
        run_import(&source_storage, &target_storage, true, &mut report)?;
        return Ok(report);
    }
    // One transaction: a conflict, or any failure, leaves the target as it was.
    target_storage.begin_batch()?;
    let outcome = run_import(&source_storage, &target_storage, false, &mut report)
        .and_then(|map| report.binding_conflict_error().map_or(Ok(map), Err));
    match outcome {
        Ok(_) => {
            target_storage.commit_batch()?;
            Ok(report)
        }
        Err(error) => {
            if let Err(rollback) = target_storage.rollback_batch() {
                return Err(VivariumError::Other(format!(
                    "{error}; the rollback also failed: {rollback}"
                )));
            }
            Err(error)
        }
    }
}

fn run_import(
    source_storage: &Storage,
    target_storage: &Storage,
    dry_run: bool,
    report: &mut MailspaceImportReport,
) -> Result<HashMap<String, String>, VivariumError> {
    let message_map = import_messages(source_storage, target_storage, dry_run, report)?;
    import_events(
        source_storage,
        target_storage,
        &message_map,
        dry_run,
        report,
    )?;
    import_links(source_storage, target_storage, dry_run, report)?;
    Ok(message_map)
}

fn import_messages(
    source_storage: &Storage,
    target_storage: &Storage,
    dry_run: bool,
    report: &mut MailspaceImportReport,
) -> Result<HashMap<String, String>, VivariumError> {
    let mut message_map = HashMap::new();
    for source_message in source_storage.list_messages()? {
        report.scanned_messages += 1;
        import_one_message(
            source_storage,
            target_storage,
            source_message,
            dry_run,
            report,
            &mut message_map,
        )?;
    }
    Ok(message_map)
}

fn import_one_message(
    source_storage: &Storage,
    target_storage: &Storage,
    source_message: StoredMessageView,
    dry_run: bool,
    report: &mut MailspaceImportReport,
    message_map: &mut HashMap<String, String>,
) -> Result<(), VivariumError> {
    if map_existing_message(target_storage, &source_message, report, message_map)? {
        return Ok(());
    }
    if let Some(conflict) = binding_conflict(target_storage, &source_message)? {
        report.binding_conflicts.push(conflict);
        return Ok(());
    }
    let blob_exists = target_storage.blob_exists(&source_message.content_id)?;
    if dry_run {
        record_dry_run_message(&source_message, blob_exists, report, message_map);
        return Ok(());
    }
    let data = source_storage.read_message(&source_message.message_id)?;
    let stored = target_storage.ingest_message(&ingest_request(&source_message), &data)?;
    if stored.created_blob {
        report.imported_blobs += 1;
    } else {
        report.deduped_blobs += 1;
    }
    report.imported_messages += 1;
    message_map.insert(source_message.message_id, stored.message_id);
    Ok(())
}

/// A source message bound to a remote position that another target message
/// holds: filing it would break the position's uniqueness.
fn binding_conflict(
    target_storage: &Storage,
    source_message: &StoredMessageView,
) -> Result<Option<String>, VivariumError> {
    let Some(remote) = &source_message.remote else {
        return Ok(None);
    };
    let holder = target_storage.remote_binding_holder(remote)?;
    Ok(holder
        .filter(|holder| *holder != source_message.message_id)
        .map(|holder| {
            format!(
                "{} (account {}) is bound to {} item {}, which {holder} already holds",
                source_message.message_id, remote.account, remote.remote_mailbox, remote.remote_uid
            )
        }))
}

fn map_existing_message(
    target_storage: &Storage,
    source_message: &StoredMessageView,
    report: &mut MailspaceImportReport,
    message_map: &mut HashMap<String, String>,
) -> Result<bool, VivariumError> {
    if let Some(existing) = target_storage.message_by_id(&source_message.message_id)? {
        if existing.content_id != source_message.content_id {
            report.conflicts.push(format!(
                "message_id {} exists with different content_id",
                source_message.message_id
            ));
            return Ok(true);
        }
        message_map.insert(source_message.message_id.clone(), existing.message_id);
        report.deduped_messages += 1;
        return Ok(true);
    }
    if let Some(existing) = target_storage.message_by_content_account_role(
        &source_message.content_id,
        &source_message.account,
        &source_message.local_role,
    )? {
        message_map.insert(source_message.message_id.clone(), existing.message_id);
        report.deduped_messages += 1;
        return Ok(true);
    }
    Ok(false)
}

fn record_dry_run_message(
    source_message: &StoredMessageView,
    blob_exists: bool,
    report: &mut MailspaceImportReport,
    message_map: &mut HashMap<String, String>,
) {
    if blob_exists {
        report.deduped_blobs += 1;
    } else {
        report.imported_blobs += 1;
    }
    report.imported_messages += 1;
    message_map.insert(
        source_message.message_id.clone(),
        source_message.message_id.clone(),
    );
}

fn ingest_request(source_message: &StoredMessageView) -> crate::storage::MessageIngestRequest {
    crate::storage::MessageIngestRequest {
        account: source_message.account.clone(),
        local_role: source_message.local_role.clone(),
        read_state: source_message.read_state,
        starred: source_message.starred,
        message_id_hint: Some(source_message.message_id.clone()),
        presented_id: None,
        seed_hint: source_message.message_id.clone(),
        remote: source_message.remote.clone(),
    }
}

fn import_events(
    source_storage: &Storage,
    target_storage: &Storage,
    message_map: &HashMap<String, String>,
    dry_run: bool,
    report: &mut MailspaceImportReport,
) -> Result<(), VivariumError> {
    for source_message in source_storage.list_messages()? {
        let Some(target_message_id) = message_map.get(&source_message.message_id) else {
            continue;
        };
        for event in source_storage.list_mailspace_events(&source_message.message_id)? {
            let input = MailspaceEventInput {
                command: event.command,
                event_type: event.event_type,
                actor_identity: event.actor_identity,
                account: event.account,
                message_id: target_message_id.clone(),
                content_id: event.content_id,
                from_role: event.from_role,
                to_role: event.to_role,
                from_identity: event.from_identity,
                to_identity: event.to_identity,
                subject: event.subject,
                note: event.note,
            };
            if target_storage.mailspace_event_exists(&input, &event.occurred_at)? {
                report.deduped_events += 1;
            } else if dry_run {
                report.imported_events += 1;
            } else {
                target_storage.append_mailspace_event_at(&input, &event.occurred_at)?;
                report.imported_events += 1;
            }
        }
    }
    Ok(())
}

fn import_links(
    source_storage: &Storage,
    target_storage: &Storage,
    dry_run: bool,
    report: &mut MailspaceImportReport,
) -> Result<(), VivariumError> {
    for link in source_storage.list_mailspace_links()? {
        if let Some(existing) = target_storage.mailspace_link_for_child(&link.child_content_id)? {
            if existing == link {
                report.deduped_links += 1;
            } else {
                report.conflicts.push(format!(
                    "link for child content_id {} points to different parent/source",
                    link.child_content_id
                ));
            }
            continue;
        }
        // A link is resolvable in the merged store when its blobs exist in the
        // target already OR will be imported from source. In dry-run the source
        // blobs are not written yet, so checking target alone false-positives
        // every link whose blobs live only in source.
        let child_present = target_storage.blob_exists(&link.child_content_id)?
            || source_storage.blob_exists(&link.child_content_id)?;
        let parent_present = target_storage.blob_exists(&link.parent_content_id)?
            || source_storage.blob_exists(&link.parent_content_id)?;
        if !child_present || !parent_present {
            report.conflicts.push(format!(
                "link for child content_id {} references missing merged blob",
                link.child_content_id
            ));
            continue;
        }
        if dry_run {
            report.imported_links += 1;
        } else {
            target_storage.link_mailspace_content(
                &link.child_content_id,
                &link.parent_content_id,
                &link.source,
            )?;
            report.imported_links += 1;
        }
    }
    Ok(())
}

fn source_mailspace_dir(path: &Path) -> Result<PathBuf, VivariumError> {
    if path.join("mailspace.toml").exists() {
        return Ok(path.to_path_buf());
    }
    let dir = path.join(".vivi");
    if dir.join("mailspace.toml").exists() {
        return Ok(dir);
    }
    Err(VivariumError::Message(format!(
        "source is not a Vivi mailspace or project root: {}",
        path.display()
    )))
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    left == right
}
