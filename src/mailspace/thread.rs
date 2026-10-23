use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;

use super::Mailspace;
use super::kind::effective_kind;
use crate::error::VivariumError;
use crate::storage::{MailspaceLink, Storage, StoredMessageView};

#[derive(Debug, Clone, Serialize)]
pub struct MailspaceThreadMessage {
    pub handle: String,
    pub message_id: String,
    pub content_id: String,
    pub account: String,
    pub role: String,
    pub kind: Option<String>,
    /// The `X-Vivi-Event` value when this letter is a lifecycle event
    /// (`done`, `reopened`, `marked`, `unmarked`, ...); null for ordinary
    /// letters. Event letters record state changes, never new thread content.
    pub event: Option<String>,
    pub date: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub body: String,
    pub parent_content_id: Option<String>,
    pub link_source: Option<String>,
    pub inferred: bool,
    /// Folded mark slugs, sorted. Empty when the item has none.
    pub marks: Vec<String>,
    /// Integration branch recorded by the item's close, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_branch: Option<String>,
    /// Commit recorded by the item's close, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_commit: Option<String>,
}

struct ThreadCandidate {
    view: StoredMessageView,
    body: String,
    kind: Option<String>,
    event: Option<String>,
    marks: Vec<String>,
    close_branch: Option<String>,
    close_commit: Option<String>,
}

/// Print a thread of messages by handle.
///
/// With `latest`, print only the newest message of the thread (the last one
/// in the default output order); a thread with no reply reports `No replies
/// yet` instead.
///
/// # Errors
/// Returns an error if the handle cannot be resolved, thread assembly fails,
/// or JSON serialization fails when `--json` is used.
pub fn print_thread(
    mailspace: &Mailspace,
    handle: &str,
    infer: bool,
    limit: usize,
    max_depth: usize,
    json: bool,
    latest: bool,
) -> Result<(), VivariumError> {
    let messages = mailspace.thread(handle, infer, limit, max_depth)?;
    if latest {
        return print_latest_thread(&messages, json);
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&messages)
                .map_err(|e| VivariumError::Other(format!("failed to encode thread JSON: {e}")))?
        );
    } else {
        print_text_thread(&messages);
        if let Some(state) = mailspace.item_event_state(handle)? {
            println!("\nState: {} (event {})", state.state, state.event);
        }
    }
    Ok(())
}

/// `--newest N`: the thread limited to its newest N messages, printed
/// oldest first. Reads the whole thread (no 50-message cap), keeps the last
/// N in date order, and renders them exactly like the default output.
/// JSON keeps the default per-message shape.
///
/// # Errors
/// Returns an error if the handle cannot be resolved, thread assembly fails,
/// or JSON serialization fails when `--json` is used.
pub fn print_thread_newest(
    mailspace: &Mailspace,
    handle: &str,
    count: usize,
    json: bool,
) -> Result<(), VivariumError> {
    if count == 0 {
        return Ok(());
    }
    let messages = mailspace.thread(handle, false, usize::MAX, usize::MAX)?;
    let start = messages.len().saturating_sub(count);
    let selected = &messages[start..];
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(selected).map_err(|e| {
                VivariumError::Other(format!("failed to encode thread JSON: {e}"))
            })?
        );
        return Ok(());
    }
    print_text_thread(selected);
    if let Some(state) = mailspace.item_event_state(handle)? {
        println!("\nState: {} (event {})", state.state, state.event);
    }
    Ok(())
}

/// `--latest`: the thread limited to its newest message. JSON keeps the
/// default shape; text prints the one per-message block, or a `No replies
/// yet` line when only the root exists.
fn print_latest_thread(
    messages: &[MailspaceThreadMessage],
    json: bool,
) -> Result<(), VivariumError> {
    if json {
        let newest = messages.len().saturating_sub(1)..;
        println!(
            "{}",
            serde_json::to_string_pretty(&messages[newest])
                .map_err(|e| VivariumError::Other(format!("failed to encode thread JSON: {e}")))?
        );
        return Ok(());
    }
    if messages.len() <= 1 {
        println!("No replies yet (thread has 1 message)");
        return Ok(());
    }
    if let Some(message) = messages.last() {
        print_text_message(message);
    }
    Ok(())
}

/// One line of a thread's table of contents: the anchor item plus each
/// reply, oldest first. Built from message rows only, so it lists every
/// reply even when the body print below is capped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadOutlineRow {
    pub date: String,
    pub handle: String,
    pub subject: String,
}

impl Mailspace {
    /// Assemble a thread of messages connected by content links.
    ///
    /// # Errors
    /// Returns an error if the handle cannot be resolved, the link map cannot
    /// be loaded, or a storage operation fails.
    pub fn thread(
        &self,
        handle: &str,
        infer: bool,
        limit: usize,
        max_depth: usize,
    ) -> Result<Vec<MailspaceThreadMessage>, VivariumError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let storage = self.storage()?;
        let seed = resolve_seed(&storage, handle)?;
        let links = if infer {
            // Inference needs every message body/handle. Explicit links do not.
            let mut links = link_map(&storage)?;
            add_inferred_links(&thread_candidates(&storage)?, &mut links);
            links
        } else {
            storage.links_for_thread(&seed.content_id)?
        };
        let included = connected_content_ids(&seed.content_id, &links, limit, max_depth);
        assemble_thread(&storage, &seed, &links, included)
    }

    /// The thread's inventory as outline rows: the anchor message first,
    /// then every linked reply oldest-first. Reads message rows only (no
    /// bodies), so the outline stays complete where the capped body print
    /// is not.
    ///
    /// # Errors
    /// Returns an error if the handle cannot be resolved, the link map
    /// cannot be loaded, or a storage operation fails.
    pub fn thread_outline(&self, handle: &str) -> Result<Vec<ThreadOutlineRow>, VivariumError> {
        let storage = self.storage()?;
        let seed = resolve_seed(&storage, handle)?;
        let links = storage.links_for_thread(&seed.content_id)?;
        let included = connected_content_ids(&seed.content_id, &links, usize::MAX, usize::MAX);
        let mut replies = Vec::with_capacity(included.len());
        for content_id in included {
            if content_id == seed.content_id {
                continue;
            }
            if let Some(view) = storage.message_by_content_id(&content_id)? {
                replies.push(view);
            }
        }
        replies.sort_by(|left, right| {
            left.date
                .cmp(&right.date)
                .then_with(|| left.content_id.cmp(&right.content_id))
        });
        let mut rows = Vec::with_capacity(replies.len() + 1);
        rows.push(outline_row(&seed));
        rows.extend(replies.iter().map(outline_row));
        Ok(rows)
    }
}

fn outline_row(view: &StoredMessageView) -> ThreadOutlineRow {
    ThreadOutlineRow {
        date: view.date.clone(),
        handle: view.handle.clone(),
        subject: view.subject.clone(),
    }
}

fn resolve_seed(storage: &Storage, handle: &str) -> Result<StoredMessageView, VivariumError> {
    let seed_id = storage.resolve_message_token(handle)?;
    storage
        .message_by_id(&seed_id)?
        .ok_or_else(|| VivariumError::Message(format!("message not found: {handle}")))
}

fn link_map(storage: &Storage) -> Result<BTreeMap<String, MailspaceLink>, VivariumError> {
    Ok(storage
        .list_mailspace_links()?
        .into_iter()
        .map(|link| (link.child_content_id.clone(), link))
        .collect())
}

fn assemble_thread(
    storage: &Storage,
    seed: &StoredMessageView,
    links: &BTreeMap<String, MailspaceLink>,
    included: BTreeSet<String>,
) -> Result<Vec<MailspaceThreadMessage>, VivariumError> {
    let mut messages = Vec::with_capacity(included.len());
    for content_id in included {
        let view = if content_id == seed.content_id {
            seed.clone()
        } else if let Some(view) = storage.message_by_content_id(&content_id)? {
            view
        } else {
            continue;
        };
        let candidate = load_candidate(storage, view)?;
        messages.push(thread_message(&candidate, links.get(&content_id)));
    }
    messages.sort_by(|left, right| {
        left.date
            .cmp(&right.date)
            .then_with(|| left.content_id.cmp(&right.content_id))
    });
    Ok(messages)
}

fn thread_candidates(storage: &Storage) -> Result<Vec<ThreadCandidate>, VivariumError> {
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for view in storage.list_messages()? {
        if !seen.insert(view.content_id.clone()) {
            continue;
        }
        candidates.push(load_candidate(storage, view)?);
    }
    Ok(candidates)
}

fn load_candidate(
    storage: &Storage,
    view: StoredMessageView,
) -> Result<ThreadCandidate, VivariumError> {
    let data = storage.read_message(&view.message_id)?;
    let events = storage.list_mailspace_events(&view.message_id)?;
    let body = text_body(&data);
    let kind = effective_kind(&view.local_role, &data, &events);
    let event = super::kind::message_event(&data);
    let marks = super::marks::folded_marks(&events);
    let metadata = storage.item_metadata(&view.message_id)?;
    Ok(ThreadCandidate {
        view,
        body,
        kind,
        event,
        marks,
        close_branch: metadata
            .get("branch")
            .filter(|value| !value.is_empty())
            .cloned(),
        close_commit: metadata
            .get("commit")
            .filter(|value| !value.is_empty())
            .cloned(),
    })
}

fn connected_content_ids(
    seed: &str,
    links: &BTreeMap<String, MailspaceLink>,
    limit: usize,
    max_depth: usize,
) -> BTreeSet<String> {
    let mut parent_of = BTreeMap::new();
    let mut children_of: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for link in links.values() {
        parent_of.insert(
            link.child_content_id.clone(),
            link.parent_content_id.clone(),
        );
        children_of
            .entry(link.parent_content_id.clone())
            .or_default()
            .push(link.child_content_id.clone());
    }
    let mut queue = VecDeque::from([(seed.to_string(), 0usize)]);
    let mut visited = BTreeSet::new();
    while let Some((content_id, depth)) = queue.pop_front() {
        // The limit is checked before the insert: every queued neighbour
        // would otherwise still be added once the thread is full.
        if visited.len() >= limit {
            break;
        }
        if !visited.insert(content_id.clone()) {
            continue;
        }
        if depth < max_depth {
            if let Some(parent) = parent_of.get(&content_id) {
                queue.push_back((parent.clone(), depth + 1));
            }
            if let Some(children) = children_of.get(&content_id) {
                queue.extend(children.iter().cloned().map(|child| (child, depth + 1)));
            }
        }
    }
    visited
}

fn add_inferred_links(candidates: &[ThreadCandidate], links: &mut BTreeMap<String, MailspaceLink>) {
    for child in candidates {
        if links.contains_key(&child.view.content_id) {
            continue;
        }
        let Some(parent) = infer_parent(child, candidates) else {
            continue;
        };
        links.insert(
            child.view.content_id.clone(),
            MailspaceLink {
                child_content_id: child.view.content_id.clone(),
                parent_content_id: parent.view.content_id.clone(),
                source: "inferred".into(),
            },
        );
    }
}

fn infer_parent<'a>(
    child: &ThreadCandidate,
    candidates: &'a [ThreadCandidate],
) -> Option<&'a ThreadCandidate> {
    let cited = candidates
        .iter()
        .filter(|candidate| candidate.view.content_id != child.view.content_id)
        .filter(|candidate| candidate.view.date <= child.view.date)
        .filter(|candidate| child.body.contains(&candidate.view.handle))
        .max_by(|left, right| left.view.date.cmp(&right.view.date));
    if cited.is_some() {
        return cited;
    }
    let subject = strip_reply_prefix(&child.view.subject);
    let subject_match = candidates
        .iter()
        .filter(|candidate| candidate.view.content_id != child.view.content_id)
        .filter(|candidate| strip_reply_prefix(&candidate.view.subject) == subject)
        .filter(|candidate| candidate.view.date <= child.view.date)
        .filter(|candidate| same_participants(&candidate.view, &child.view))
        .max_by(|left, right| left.view.date.cmp(&right.view.date));
    if subject_match.is_some() {
        return subject_match;
    }
    None
}

fn same_participants(left: &StoredMessageView, right: &StoredMessageView) -> bool {
    participants(left) == participants(right)
}

fn participants(view: &StoredMessageView) -> BTreeSet<String> {
    let mut participants = BTreeSet::from([view.from_addr.to_ascii_lowercase()]);
    participants.extend(view.to_addr.split(", ").map(str::to_ascii_lowercase));
    participants.extend(view.cc_addr.split(", ").map(str::to_ascii_lowercase));
    participants
}

fn thread_message(
    candidate: &ThreadCandidate,
    link: Option<&MailspaceLink>,
) -> MailspaceThreadMessage {
    MailspaceThreadMessage {
        handle: candidate.view.handle.clone(),
        message_id: candidate.view.message_id.clone(),
        content_id: candidate.view.content_id.clone(),
        account: candidate.view.account.clone(),
        role: candidate.view.local_role.clone(),
        kind: candidate.kind.clone(),
        event: candidate.event.clone(),
        date: candidate.view.date.clone(),
        from: candidate.view.from_addr.clone(),
        to: candidate.view.to_addr.clone(),
        cc: candidate.view.cc_addr.clone(),
        subject: candidate.view.subject.clone(),
        body: candidate.body.clone(),
        parent_content_id: link.map(|link| link.parent_content_id.clone()),
        link_source: link.map(|link| link.source.clone()),
        inferred: link.is_some_and(|link| link.source == "inferred"),
        marks: candidate.marks.clone(),
        close_branch: candidate.close_branch.clone(),
        close_commit: candidate.close_commit.clone(),
    }
}

fn print_text_thread(messages: &[MailspaceThreadMessage]) {
    if messages.is_empty() {
        println!("No matching thread messages.");
        return;
    }
    println!("Thread ({} message(s))", messages.len());
    for message in messages {
        print_text_message(message);
    }
}

/// Print the table of contents heading a `need show` / `want show` thread:
/// one line for the anchor item, then one indented line per reply. A reader
/// sees the full inventory even when the capped thread below stops early.
pub fn print_thread_outline(rows: &[ThreadOutlineRow]) {
    println!("Contents ({} message(s))\n", rows.len());
    for (index, row) in rows.iter().enumerate() {
        let subject: String = row.subject.chars().take(100).collect();
        let indent = if index == 0 { "" } else { "  " };
        println!("{indent}{} {} {}", row.date, row.handle, subject);
    }
    println!();
}

fn print_text_message(message: &MailspaceThreadMessage) {
    let source = message
        .link_source
        .as_deref()
        .map(|source| format!(" {source}"))
        .unwrap_or_default();
    println!(
        "\n## {} - {} [{}{}]",
        message.date,
        message.handle,
        message.kind.as_deref().unwrap_or("mail"),
        source
    );
    println!("From: {}", message.from);
    println!("To: {}", message.to);
    println!("Content: {}", message.content_id);
    println!("Subject: {}", message.subject);
    if !message.marks.is_empty() {
        println!("Marks: {}", message.marks.join(" "));
    }
    if let (Some(branch), Some(commit)) = (&message.close_branch, &message.close_commit) {
        println!("Done on: {branch} {commit}");
    }
    println!();
    println!("{}", message.body.trim());
}

/// The text body of a stored message with LF line ends. Compose stores bodies
/// CR LF; every reader of a thread body sees LF.
pub(super) fn text_body(data: &[u8]) -> String {
    mail_parser::MessageParser::default()
        .parse(data)
        .and_then(|parsed| parsed.body_text(0).map(|body| body.replace("\r\n", "\n")))
        .unwrap_or_default()
}

fn strip_reply_prefix(subject: &str) -> String {
    let mut subject = subject.trim();
    loop {
        let lower = subject.to_ascii_lowercase();
        let Some(after_re) = lower.strip_prefix("re") else {
            break;
        };
        let Some(colon_offset) = reply_prefix_colon(after_re) else {
            break;
        };
        subject = subject[2 + colon_offset + 1..].trim_start();
    }
    subject.to_ascii_lowercase()
}

fn reply_prefix_colon(after_re: &str) -> Option<usize> {
    if after_re.starts_with(':') {
        return Some(0);
    }
    let closing = after_re.strip_prefix('[')?.find(']')? + 2;
    after_re.get(closing..)?.starts_with(':').then_some(closing)
}

#[cfg(test)]
#[path = "thread_test.rs"]
mod tests;
