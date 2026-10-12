//! Compose and parse letters for the MCP surface, reusing vivi's
//! `build_compose_draft` so outbound bytes match the CLI. The server mints
//! the traveling id (decided item 15). Event headers follow vivi's
//! `X-Vivi-Event` / `Of` / `Prev` shape so a checkout that pulls the letter
//! folds the same state.

use std::collections::{HashMap, HashSet};

use vivi::message::{ComposeDraft, build_compose_draft, normalize_message_id};

use crate::auth::AuthError;
use crate::identities::IdentityRecord;
use crate::projects::{AppendItem, AppendOutcome, ProjectError, ProjectRecord, ProjectStore};

const HANDLE_LEN: usize = 8;

#[derive(Debug, Clone, Default)]
pub struct ParsedLetter {
    pub from: String,
    pub from_domain: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub rfc_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub kind_header: Option<String>,
    pub event: Option<String>,
    pub event_of: Option<String>,
    pub event_prev: Vec<String>,
    pub depends_on: Vec<String>,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct ItemState {
    pub state: String,
    pub event: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ComposedLetter {
    pub traveling_id: String,
    pub raw_eml: Vec<u8>,
    pub to: String,
    pub kind: Option<String>,
}

pub fn caller_address(identity: &IdentityRecord) -> Result<String, String> {
    let prefix = identity.mailspace_prefix().ok_or_else(|| {
        "this identity has no mailspace prefix; it cannot send from MCP".to_owned()
    })?;
    Ok(format!("{}@{prefix}.local", identity.name))
}

pub fn mint_traveling_id() -> Result<String, AuthError> {
    let mut raw = [0u8; 12];
    getrandom::fill(&mut raw).map_err(|_| AuthError::Entropy)?;
    Ok(format!("msg_{}", hex_encode(&raw)))
}

pub fn parse_letter(raw: &[u8]) -> ParsedLetter {
    let text = String::from_utf8_lossy(raw);
    let (header_text, body) = split_headers(&text);
    let mut parsed = ParsedLetter {
        body: body.to_owned(),
        ..ParsedLetter::default()
    };
    for (name, value) in folded_headers(header_text) {
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "from" => {
                parsed.from = value.clone();
                parsed.from_domain = address_domain(&value);
            }
            "to" => parsed.to = split_addresses(&value),
            "cc" => parsed.cc = split_addresses(&value),
            "subject" => parsed.subject = value,
            "message-id" => parsed.rfc_id = normalize_message_id(&value),
            "in-reply-to" => parsed.in_reply_to = normalize_message_id(&value),
            "references" => {
                parsed.references = value
                    .split_whitespace()
                    .filter_map(normalize_message_id)
                    .collect();
            }
            "x-vivi-kind" => parsed.kind_header = Some(value.to_ascii_lowercase()),
            "x-vivi-event" => parsed.event = Some(value.to_ascii_lowercase()),
            "x-vivi-event-of" => parsed.event_of = normalize_message_id(&value),
            "x-vivi-event-prev" => {
                if let Some(id) = normalize_message_id(&value) {
                    parsed.event_prev.push(id);
                }
            }
            "x-vivi-depends-on" => parsed.depends_on.push(value),
            _ => {}
        }
    }
    parsed
}

pub fn compose_send(
    identity: &IdentityRecord,
    to: &[String],
    subject: &str,
    body: &str,
    kind: &str,
    depends_on: &[String],
) -> Result<ComposedLetter, String> {
    let from = caller_address(identity)?;
    if to.is_empty() {
        return Err("send needs at least one to address".to_owned());
    }
    let traveling_id = mint_traveling_id().map_err(|_| "entropy unavailable".to_owned())?;
    let prefix = identity.mailspace_prefix().unwrap_or("server");
    let mut eml = build_compose_draft(&ComposeDraft {
        from,
        to: to.to_vec(),
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: subject.to_owned(),
        body: body.to_owned(),
        html_body: None,
    })
    .map_err(|e| e.to_string())?;
    eml = set_header(
        &eml,
        "Message-ID",
        &format!("<{traveling_id}@{prefix}.local>"),
    )?;
    eml = set_header(&eml, "X-Vivi-Kind", kind)?;
    for dep in depends_on {
        eml = add_header(&eml, "X-Vivi-Depends-On", dep)?;
    }
    let envelope_kind = match kind {
        "need" | "task" | "want" => Some(kind.to_owned()),
        _ => None,
    };
    Ok(ComposedLetter {
        traveling_id,
        raw_eml: eml.into_bytes(),
        to: to.join(","),
        kind: envelope_kind,
    })
}

pub fn compose_reply(
    identity: &IdentityRecord,
    parent: &ProjectRecord,
    body: &str,
    event: Option<(&str, Option<&str>)>,
) -> Result<ComposedLetter, String> {
    let from = caller_address(identity)?;
    let parsed = parse_letter(&parent.raw_eml);
    let mut to = Vec::new();
    push_unique(&mut to, parent_from_address(&parsed, parent));
    for address in parsed.to.iter().chain(parsed.cc.iter()) {
        if !address.eq_ignore_ascii_case(&from) {
            push_unique(&mut to, address.clone());
        }
    }
    if to.is_empty() {
        to.push(from.clone());
    }
    let subject = reply_subject(&parsed.subject);
    let traveling_id = mint_traveling_id().map_err(|_| "entropy unavailable".to_owned())?;
    let prefix = identity.mailspace_prefix().unwrap_or("server");
    let mut eml = build_compose_draft(&ComposeDraft {
        from,
        to: to.clone(),
        cc: Vec::new(),
        bcc: Vec::new(),
        subject,
        body: body.to_owned(),
        html_body: None,
    })
    .map_err(|e| e.to_string())?;
    eml = set_header(
        &eml,
        "Message-ID",
        &format!("<{traveling_id}@{prefix}.local>"),
    )?;
    eml = set_header(&eml, "X-Vivi-Kind", "mail")?;
    if let Some(rfc_id) = parsed
        .rfc_id
        .as_deref()
        .or(Some(parent.message_id.as_str()))
    {
        eml = set_header(&eml, "In-Reply-To", &format!("<{rfc_id}>"))?;
        let mut refs = parsed.references.clone();
        if !refs.iter().any(|r| r == rfc_id) {
            refs.push(rfc_id.to_owned());
        }
        if !refs.is_empty() {
            let value = refs
                .iter()
                .map(|id| format!("<{id}>"))
                .collect::<Vec<_>>()
                .join(" ");
            eml = set_header(&eml, "References", &value)?;
        }
    }
    if let Some((kind, prev)) = event {
        let of = parsed
            .rfc_id
            .clone()
            .unwrap_or_else(|| parent.message_id.clone());
        eml = set_header(&eml, "X-Vivi-Event", kind)?;
        eml = set_header(&eml, "X-Vivi-Event-Of", &format!("<{of}>"))?;
        if let Some(prev) = prev {
            eml = add_header(&eml, "X-Vivi-Event-Prev", &format!("<{prev}>"))?;
        }
    }
    Ok(ComposedLetter {
        traveling_id,
        raw_eml: eml.into_bytes(),
        to: to.join(","),
        kind: None,
    })
}

pub fn append_composed(
    store: &ProjectStore,
    project_id: &str,
    identity: &IdentityRecord,
    composed: ComposedLetter,
) -> Result<AppendOutcome, ProjectError> {
    let item = AppendItem::new(
        &composed.traveling_id,
        &identity.name,
        Some(&composed.to),
        composed.kind.as_deref(),
        composed.raw_eml,
    )?
    .with_from_prefix(identity.mailspace_prefix());
    store.append(project_id, &item)
}

/// Fold authorized event letters onto their items. An event counts when its
/// From domain equals the item's From domain (vivi's receive-side rule).
pub fn fold_item_states(records: &[ProjectRecord]) -> HashMap<String, ItemState> {
    let parsed: Vec<(&ProjectRecord, ParsedLetter)> = records
        .iter()
        .map(|record| (record, parse_letter(&record.raw_eml)))
        .collect();
    let mut item_domain: HashMap<String, String> = HashMap::new();
    for (record, letter) in &parsed {
        if let Some(rfc_id) = letter.rfc_id.as_deref() {
            if let Some(domain) = letter.from_domain.as_deref() {
                item_domain.insert(rfc_id.to_owned(), domain.to_owned());
            }
        }
        if let Some(domain) = letter.from_domain.as_deref() {
            item_domain.insert(record.message_id.clone(), domain.to_owned());
        }
    }
    let mut events: HashMap<String, Vec<EventRow>> = HashMap::new();
    for (record, letter) in &parsed {
        let Some(kind) = letter.event.as_deref() else {
            continue;
        };
        if !matches!(kind, "done" | "reopened") {
            continue;
        }
        let Some(of) = letter.event_of.clone() else {
            continue;
        };
        let Some(event_id) = letter.rfc_id.clone() else {
            continue;
        };
        let Some(from_domain) = letter.from_domain.as_deref() else {
            continue;
        };
        let item_dom = item_domain.get(&of).map(String::as_str);
        if item_dom.is_some_and(|domain| !domain.eq_ignore_ascii_case(from_domain)) {
            continue;
        }
        events.entry(of).or_default().push(EventRow {
            id: event_id,
            kind: kind.to_owned(),
            prev: letter.event_prev.clone(),
            appended_at: record.appended_at.clone(),
        });
    }
    let mut out = HashMap::new();
    for (of, chain) in events {
        let named: HashSet<&str> = chain
            .iter()
            .flat_map(|e| e.prev.iter().map(String::as_str))
            .collect();
        let mut heads: Vec<&EventRow> = chain
            .iter()
            .filter(|event| !named.contains(event.id.as_str()))
            .collect();
        if heads.is_empty() {
            heads = chain.iter().collect();
        }
        heads.sort_by(|a, b| (&a.appended_at, &a.id).cmp(&(&b.appended_at, &b.id)));
        if let Some(head) = heads.pop() {
            let state = if head.kind == "done" { "done" } else { "open" };
            out.insert(
                of,
                ItemState {
                    state: state.to_owned(),
                    event: Some(head.id.clone()),
                },
            );
        }
    }
    out
}

pub fn item_status(
    record: &ProjectRecord,
    letter: &ParsedLetter,
    states: &HashMap<String, ItemState>,
) -> String {
    if record.kind.is_none() {
        return "mail".to_owned();
    }
    if let Some(rfc_id) = letter.rfc_id.as_deref()
        && let Some(state) = states.get(rfc_id)
    {
        return state.state.clone();
    }
    if let Some(state) = states.get(&record.message_id) {
        return state.state.clone();
    }
    "open".to_owned()
}

pub fn handle_of(message_id: &str) -> Option<&str> {
    message_id
        .get(4..4 + HANDLE_LEN)
        .filter(|h| h.len() == HANDLE_LEN && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub fn valid_handle(handle: &str) -> bool {
    handle.len() == HANDLE_LEN && handle.bytes().all(|b| b.is_ascii_hexdigit())
}

struct EventRow {
    id: String,
    kind: String,
    prev: Vec<String>,
    appended_at: String,
}

fn parent_from_address(parsed: &ParsedLetter, parent: &ProjectRecord) -> String {
    if let Some(address) = email_address(&parsed.from) {
        return address;
    }
    if let Some(prefix) = &parent.from_prefix {
        return format!("{}@{prefix}.local", parent.from);
    }
    parent.from.clone()
}

fn email_address(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if let Some(start) = trimmed.find('<')
        && let Some(end) = trimmed[start + 1..].find('>')
    {
        return Some(trimmed[start + 1..start + 1 + end].to_owned());
    }
    trimmed.contains('@').then(|| trimmed.to_owned())
}

fn address_domain(raw: &str) -> Option<String> {
    let address = email_address(raw)?;
    address
        .rsplit_once('@')
        .map(|(_, domain)| domain.trim().to_ascii_lowercase())
}

fn split_addresses(raw: &str) -> Vec<String> {
    raw.split(',')
        .filter_map(|part| email_address(part))
        .collect()
}

fn split_headers(text: &str) -> (&str, &str) {
    if let Some(pos) = text.find("\r\n\r\n") {
        return (&text[..pos], &text[pos + 4..]);
    }
    if let Some(pos) = text.find("\n\n") {
        return (&text[..pos], &text[pos + 2..]);
    }
    (text, "")
}

fn folded_headers(header_text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut name = String::new();
    let mut value = String::new();
    for line in header_text.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            value.push(' ');
            value.push_str(line.trim());
            continue;
        }
        if !name.is_empty() {
            out.push((std::mem::take(&mut name), value.trim().to_owned()));
        }
        match line.split_once(':') {
            Some((n, v)) => {
                name = n.trim().to_owned();
                value = v.trim().to_owned();
            }
            None => {
                name.clear();
                value.clear();
            }
        }
    }
    if !name.is_empty() {
        out.push((name, value.trim().to_owned()));
    }
    out
}

fn add_header(eml: &str, name: &str, value: &str) -> Result<String, String> {
    let newline = if eml.contains("\r\n") { "\r\n" } else { "\n" };
    let separator = format!("{newline}{newline}");
    let (headers, body) = eml
        .split_once(&separator)
        .ok_or_else(|| "composed letter has no header/body separator".to_owned())?;
    Ok(format!(
        "{headers}{newline}{name}: {value}{separator}{body}"
    ))
}

fn set_header(eml: &str, name: &str, value: &str) -> Result<String, String> {
    let newline = if eml.contains("\r\n") { "\r\n" } else { "\n" };
    let separator = format!("{newline}{newline}");
    let (headers, body) = eml
        .split_once(&separator)
        .ok_or_else(|| "composed letter has no header/body separator".to_owned())?;
    let needle = format!("{name}:");
    let mut replaced = false;
    let mut out = String::new();
    for line in headers.lines() {
        if !replaced
            && line.len() >= needle.len()
            && line[..needle.len()].eq_ignore_ascii_case(&needle)
        {
            if !out.is_empty() {
                out.push_str(newline);
            }
            out.push_str(name);
            out.push_str(": ");
            out.push_str(value);
            replaced = true;
        } else {
            if !out.is_empty() {
                out.push_str(newline);
            }
            out.push_str(line);
        }
    }
    if !replaced {
        out.push_str(newline);
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
    }
    Ok(format!("{out}{separator}{body}"))
}

fn reply_subject(subject: &str) -> String {
    let trimmed = subject.trim();
    if trimmed.is_empty() {
        "Re: (no subject)".to_owned()
    } else if trimmed.len() >= 3 && trimmed[..3].eq_ignore_ascii_case("re:") {
        trimmed.to_owned()
    } else {
        format!("Re: {trimmed}")
    }
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if !value.is_empty() && !list.iter().any(|seen| seen.eq_ignore_ascii_case(&value)) {
        list.push(value);
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vivi::storage::is_traveling_id;

    #[test]
    fn traveling_ids_match_vivi_shape() {
        let id = mint_traveling_id().unwrap();
        assert!(is_traveling_id(&id), "{id}");
        assert_eq!(handle_of(&id).unwrap().len(), 8);
    }

    #[test]
    fn fold_prefers_the_unreferenced_head() {
        let item = ProjectRecord {
            sequence: 1,
            message_id: "msg_aaaaaaaa0000000000000001".into(),
            from: "alice".into(),
            from_prefix: Some("aaaaaaaa".into()),
            to: Some("hand@bbbbbbbb.local".into()),
            kind: Some("need".into()),
            content_sha256: "x".into(),
            appended_at: "2026-10-12T00:00:00Z".into(),
            raw_eml: b"From: alice@aaaaaaaa.local\nTo: hand@bbbbbbbb.local\n\
                 Message-ID: <need-1@aaaaaaaa.local>\nSubject: s\n\nbody\n"
                .to_vec(),
        };
        let done = ProjectRecord {
            sequence: 2,
            message_id: "msg_aaaaaaaa0000000000000002".into(),
            from: "alice".into(),
            from_prefix: Some("aaaaaaaa".into()),
            to: Some("hand@bbbbbbbb.local".into()),
            kind: None,
            content_sha256: "y".into(),
            appended_at: "2026-10-12T00:00:01Z".into(),
            raw_eml: b"From: alice@aaaaaaaa.local\nTo: hand@bbbbbbbb.local\n\
                 Message-ID: <done-1@aaaaaaaa.local>\nIn-Reply-To: <need-1@aaaaaaaa.local>\n\
                 X-Vivi-Event: done\nX-Vivi-Event-Of: <need-1@aaaaaaaa.local>\n\ndone\n"
                .to_vec(),
        };
        let states = fold_item_states(&[item, done]);
        assert_eq!(states["need-1@aaaaaaaa.local"].state, "done");
    }

    #[test]
    fn foreign_domain_event_does_not_close() {
        let item = ProjectRecord {
            sequence: 1,
            message_id: "msg_aaaaaaaa0000000000000001".into(),
            from: "alice".into(),
            from_prefix: Some("aaaaaaaa".into()),
            to: Some("hand@bbbbbbbb.local".into()),
            kind: Some("task".into()),
            content_sha256: "x".into(),
            appended_at: "2026-10-12T00:00:00Z".into(),
            raw_eml: b"From: alice@aaaaaaaa.local\nTo: hand@bbbbbbbb.local\n\
                 Message-ID: <task-1@aaaaaaaa.local>\nSubject: s\n\nbody\n"
                .to_vec(),
        };
        let report = ProjectRecord {
            sequence: 2,
            message_id: "msg_bbbbbbbb0000000000000002".into(),
            from: "bob".into(),
            from_prefix: Some("bbbbbbbb".into()),
            to: Some("alice@aaaaaaaa.local".into()),
            kind: None,
            content_sha256: "y".into(),
            appended_at: "2026-10-12T00:00:01Z".into(),
            raw_eml: b"From: bob@bbbbbbbb.local\nTo: alice@aaaaaaaa.local\n\
                 Message-ID: <rep-1@bbbbbbbb.local>\nIn-Reply-To: <task-1@aaaaaaaa.local>\n\
                 X-Vivi-Event: done\nX-Vivi-Event-Of: <task-1@aaaaaaaa.local>\n\ndone\n"
                .to_vec(),
        };
        let states = fold_item_states(&[item, report]);
        assert!(!states.contains_key("task-1@aaaaaaaa.local"));
    }
}
