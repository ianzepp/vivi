use crate::storage::MailspaceEvent;

pub(super) fn message_kind(data: &[u8]) -> Option<String> {
    let raw = std::str::from_utf8(data).ok()?;
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            return None;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("X-Vivi-Kind") {
            return Some(value.trim().to_ascii_lowercase());
        }
    }
    None
}

/// The kind derivable without the blob: the role's own kind, or the kind
/// named by the lifecycle commands on the item's event log. Every filed
/// work item carries its `need|task|want send` event and every finished one
/// its `... done` event, so `done` and `sent` rows are decided here without
/// reading the message body.
pub(crate) fn events_kind(role: &str, events: &[MailspaceEvent]) -> Option<String> {
    match role {
        "tasks" => return Some("task".into()),
        "needs" => return Some("need".into()),
        "wants" => return Some("want".into()),
        "memos" => return Some("memo".into()),
        "rules" => return Some("rule".into()),
        _ => {}
    }
    // The newest deciding event wins: a want promoted to a need carries its
    // old `want send` behind the later `need done`, and the item is a need.
    for event in events.iter().rev() {
        let kind = match event.command.as_str() {
            "need send" | "need done" => "need",
            "task send" | "task done" => "task",
            "want send" | "want done" | "want drop" => "want",
            _ => continue,
        };
        return Some(kind.into());
    }
    None
}

pub(crate) fn effective_kind(role: &str, data: &[u8], events: &[MailspaceEvent]) -> Option<String> {
    events_kind(role, events).or_else(|| message_kind(data))
}

/// Kind filtering without the blob: the role and event log decide. `None`
/// kinds match a `mail` filter the way a header-less body does.
pub(super) fn matches_kind_events(role: &str, events: &[MailspaceEvent], kind: &str) -> bool {
    match (kind, events_kind(role, events).as_deref()) {
        ("mail", None | Some("mail")) => true,
        (kind, Some(found)) => found == kind,
        _ => false,
    }
}

pub(super) fn matches_kind(role: &str, data: &[u8], events: &[MailspaceEvent], kind: &str) -> bool {
    match (kind, effective_kind(role, data, events).as_deref()) {
        ("mail", None | Some("mail")) => true,
        (kind, Some(found)) => found == kind,
        _ => false,
    }
}
