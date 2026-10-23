//! The flow kind of a need: the record front matter kind, then marks.

use super::def::FlowKind;
use crate::letter::read::record_of;

/// The kind of a need's runs.
///
/// The `kind` of the open record's front matter wins. A body that fails to
/// parse, or a record with no kind, falls to the marks: `research`, then
/// `release`, then `repair`, else build.
#[must_use]
pub fn of(record_body: Option<&str>, marks: &[String]) -> FlowKind {
    if let Some(body) = record_body {
        if let Ok(envelope) = record_of("", body) {
            if let Some(kind) = envelope.front.kind {
                return kind.flow_kind();
            }
        }
    }
    let has = |mark: &str| marks.iter().any(|m| m == mark);
    if has("research") {
        FlowKind::Research
    } else if has("release") {
        FlowKind::Release
    } else if has("repair") {
        FlowKind::Repair
    } else {
        FlowKind::Build
    }
}

#[cfg(test)]
#[path = "kind_test.rs"]
mod tests;
