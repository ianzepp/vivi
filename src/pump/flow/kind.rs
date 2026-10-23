//! The flow kind of a need: record kind line, then marks.

use super::def::FlowKind;
use crate::letter::LetterError;
use crate::letter::read::record_of;

/// The kind of a need's runs.
///
/// The `kind` of the open record's front matter wins. A body that fails to
/// parse falls to the marks; a body with no front matter uses the old
/// `kind: <word>` line when the word is a kind. Otherwise the marks decide: `research`, then `release`, then
/// `repair`, else build. The second value is the record's word when it named
/// no kind, so the caller can raise `flow-kind-unknown`.
#[must_use]
pub fn of(record_body: Option<&str>, marks: &[String]) -> (FlowKind, Option<String>) {
    let mut unknown = None;
    if let Some(body) = record_body {
        match record_of("", body) {
            Ok(envelope) => {
                if let Some(kind) = envelope.front.kind {
                    return (kind.flow_kind(), None);
                }
            }
            Err(LetterError::Legacy { .. }) => {
                // TRANSITIONAL (W3-B3): a record with no front matter names its kind on a line.
                if let Some(word) = kind_line(body) {
                    match FlowKind::parse(word) {
                        Some(kind) => return (kind, None),
                        None => unknown = Some(word.to_string()),
                    }
                }
            }
            Err(_) => {}
        }
    }
    let has = |mark: &str| marks.iter().any(|m| m == mark);
    let kind = if has("research") {
        FlowKind::Research
    } else if has("release") {
        FlowKind::Release
    } else if has("repair") {
        FlowKind::Repair
    } else {
        FlowKind::Build
    };
    (kind, unknown)
}

fn kind_line(body: &str) -> Option<&str> {
    body.lines()
        .find_map(|line| line.trim().strip_prefix("kind:"))
        .map(str::trim)
        .filter(|word| !word.is_empty())
}

#[cfg(test)]
#[path = "kind_test.rs"]
mod tests;
