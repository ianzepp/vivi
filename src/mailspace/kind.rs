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

pub(crate) fn effective_kind(role: &str, data: &[u8], events: &[MailspaceEvent]) -> Option<String> {
    match role {
        "tasks" => return Some("task".into()),
        "needs" => return Some("need".into()),
        "wants" => return Some("want".into()),
        "memos" => return Some("memo".into()),
        "rules" => return Some("rule".into()),
        _ => {}
    }
    if events.iter().any(|event| event.command == "need done") {
        return Some("need".into());
    }
    if events.iter().any(|event| event.command == "task done") {
        return Some("task".into());
    }
    message_kind(data)
}

pub(super) fn matches_kind(role: &str, data: &[u8], events: &[MailspaceEvent], kind: &str) -> bool {
    match (kind, effective_kind(role, data, events).as_deref()) {
        ("mail", None | Some("mail")) => true,
        (kind, Some(found)) => found == kind,
        _ => false,
    }
}
