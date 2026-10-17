use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::Mailspace;
use crate::error::VivariumError;
use crate::storage::{Storage, StoredMessageView};

#[derive(Debug, Serialize)]
struct ArchiveFrontmatter {
    message_id: String,
    content_id: String,
    handle: String,
    kind: String,
    project: String,
    account: String,
    from: String,
    to: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    cc: String,
    subject: String,
    date: String,
    role: String,
    absorbed_at: String,
    absorbed_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_content_id: Option<String>,
    events: Vec<ArchiveEvent>,
}

#[derive(Debug, Serialize)]
struct ArchiveEvent {
    occurred_at: String,
    command: String,
    event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

pub(super) fn export_absorbed(
    mailspace: &Mailspace,
    message_id: &str,
    kind: &str,
) -> Result<(), VivariumError> {
    let Some(raw) = mailspace.config.archive.as_deref() else {
        return Ok(());
    };
    let archive = resolve_archive_path(&mailspace.root, raw)?;
    validate_archive_repo(&archive)?;
    let storage = mailspace.storage()?;
    let Some(view) = storage.message_by_id(message_id)? else {
        return Err(VivariumError::Message(format!(
            "message not found: {message_id}"
        )));
    };
    let rendered = render_record(mailspace, &storage, &view, kind)?;
    write_if_changed(
        &archive_file_path(&archive, &mailspace.config.name, kind, &view.message_id),
        &rendered,
    )?;
    Ok(())
}

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ArchiveExportReport {
    pub scanned: usize,
    pub written: usize,
    pub unchanged: usize,
}

pub(super) fn export_all(mailspace: &Mailspace) -> Result<ArchiveExportReport, VivariumError> {
    let raw = mailspace.config.archive.as_deref().ok_or_else(|| {
        VivariumError::Message(
            "no archive configured; set one with `vivi mailspace archive --set <path>`".into(),
        )
    })?;
    let archive = resolve_archive_path(&mailspace.root, raw)?;
    validate_archive_repo(&archive)?;
    let storage = mailspace.storage()?;
    let mut report = ArchiveExportReport::default();
    for view in storage.list_messages()? {
        let Some(kind) = archive_kind(mailspace, &view)? else {
            continue;
        };
        report.scanned += 1;
        let rendered = render_record(mailspace, &storage, &view, &kind)?;
        let path = archive_file_path(&archive, &mailspace.config.name, &kind, &view.message_id);
        if write_if_changed(&path, &rendered)? {
            report.written += 1;
        } else {
            report.unchanged += 1;
        }
    }
    Ok(report)
}

fn archive_kind(
    mailspace: &Mailspace,
    view: &StoredMessageView,
) -> Result<Option<String>, VivariumError> {
    if view.absorbed_at.is_none() && view.local_role != "done" {
        return Ok(None);
    }
    let kind = mailspace.source_kind(&*mailspace.storage()?, view)?;
    if view.absorbed_at.is_some()
        || (view.local_role == "done" && matches!(kind.as_str(), "task" | "need" | "want"))
    {
        return Ok(Some(kind));
    }
    Ok(None)
}

fn expand_tilde(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => match dirs::home_dir() {
            Some(home) => home.join(rest),
            None => PathBuf::from(path),
        },
        None => PathBuf::from(path),
    }
}

pub(super) fn resolve_archive_path(root: &Path, raw: &str) -> Result<PathBuf, VivariumError> {
    let expanded = expand_tilde(raw.trim());
    let path = if expanded.is_absolute() {
        expanded
    } else {
        root.join(expanded)
    };
    path.canonicalize().map_err(|e| {
        VivariumError::Message(format!(
            "archive path '{}' does not exist: {e}",
            path.display()
        ))
    })
}

pub(super) fn validate_archive_repo(path: &Path) -> Result<(), VivariumError> {
    if !path.is_dir() {
        return Err(VivariumError::Message(format!(
            "archive path is not a directory: {}",
            path.display()
        )));
    }
    if !path.join(".git").exists() {
        return Err(VivariumError::Message(format!(
            "archive must be a git repository: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(super) fn render_record(
    mailspace: &Mailspace,
    storage: &Storage,
    view: &StoredMessageView,
    kind: &str,
) -> Result<String, VivariumError> {
    let events = storage
        .list_mailspace_events(&view.message_id)?
        .into_iter()
        .map(|event| ArchiveEvent {
            occurred_at: event.occurred_at,
            command: event.command,
            event_type: event.event_type,
            actor: event.actor_identity,
            note: event.note,
        })
        .collect();
    let parent_content_id = storage
        .list_mailspace_links_for_children(std::slice::from_ref(&view.content_id))?
        .get(&view.content_id)
        .map(|link| link.parent_content_id.clone());
    let frontmatter = ArchiveFrontmatter {
        message_id: view.message_id.clone(),
        content_id: view.content_id.clone(),
        handle: view.handle.clone(),
        kind: kind.into(),
        project: mailspace.config.name.clone(),
        account: view.account.clone(),
        from: view.from_addr.clone(),
        to: view.to_addr.clone(),
        cc: view.cc_addr.clone(),
        subject: view.subject.clone(),
        date: view.date.clone(),
        role: view.local_role.clone(),
        absorbed_at: view.absorbed_at.clone().unwrap_or_default(),
        absorbed_by: view.absorbed_by.clone().unwrap_or_default(),
        parent_content_id,
        events,
    };
    let toml = toml::to_string(&frontmatter)
        .map_err(|e| VivariumError::Other(format!("failed to encode archive frontmatter: {e}")))?;
    Ok(format!(
        "+++\n{}+++\n\n{}\n",
        toml,
        text_body(&storage.read_message(&view.message_id)?)
    ))
}

pub(super) fn archive_file_path(
    archive: &Path,
    project: &str,
    kind: &str,
    message_id: &str,
) -> PathBuf {
    archive
        .join(project)
        .join(kind)
        .join(format!("{}.md", sanitize_name(message_id)))
}

pub(super) fn write_if_changed(path: &Path, contents: &str) -> Result<bool, VivariumError> {
    if path.exists() && fs::read_to_string(path)? == contents {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(true)
}

pub(super) fn text_body(data: &[u8]) -> String {
    mail_parser::MessageParser::default()
        .parse(data)
        .and_then(|parsed| parsed.body_text(0).map(|body| body.to_string()))
        .unwrap_or_default()
}

pub(super) fn sanitize_name(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            out.push(ch);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() { "record".into() } else { out }
}
