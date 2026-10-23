//! The flow kind of a need: record kind line, then marks.

use super::def::FlowKind;

/// The kind of a need's runs.
///
/// A `kind: <word>` line in the open record body wins when the word is a
/// kind. Otherwise the marks decide: `research`, then `release`, then
/// `repair`, else build. The second value is the record's word when it named
/// no kind, so the caller can raise `flow-kind-unknown`.
#[must_use]
pub fn of(record_body: Option<&str>, marks: &[String]) -> (FlowKind, Option<String>) {
    let mut unknown = None;
    if let Some(word) = record_body.and_then(kind_line) {
        match FlowKind::parse(word) {
            Some(kind) => return (kind, None),
            None => unknown = Some(word.to_string()),
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
