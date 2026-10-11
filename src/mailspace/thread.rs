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
}

struct ThreadCandidate {
    view: StoredMessageView,
    body: String,
    kind: Option<String>,
    marks: Vec<String>,
}

/// Print a thread of messages by handle.
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
) -> Result<(), VivariumError> {
    let messages = mailspace.thread(handle, infer, limit, max_depth)?;
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
        let mut links = link_map(&storage)?;
        if infer {
            // Inference needs every message body/handle. Explicit links do not.
            add_inferred_links(&thread_candidates(&storage)?, &mut links);
        }
        let included = connected_content_ids(&seed.content_id, &links, limit, max_depth);
        assemble_thread(&storage, &seed, &links, included)
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
    let marks = super::marks::folded_marks(&events);
    Ok(ThreadCandidate {
        view,
        body,
        kind,
        marks,
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
    }
}

fn print_text_thread(messages: &[MailspaceThreadMessage]) {
    if messages.is_empty() {
        println!("No matching thread messages.");
        return;
    }
    println!("Thread ({} message(s))", messages.len());
    for message in messages {
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
        println!();
        println!("{}", message.body.trim());
    }
}

pub(super) fn text_body(data: &[u8]) -> String {
    mail_parser::MessageParser::default()
        .parse(data)
        .and_then(|parsed| parsed.body_text(0).map(|body| body.to_string()))
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
