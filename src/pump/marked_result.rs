//! Locate a worker's marked JSON result inside its receipt text.
//!
//! The primary form is a line `<PREFIX>{json}`. Models sometimes drop the
//! prefix, so when no prefixed line parses, the last line that is a
//! standalone JSON object (optionally wrapped in backticks) and that parses
//! as the requested type is accepted. Only the line is located here; the
//! caller validates the parsed value exactly as for a prefixed line.

use serde::de::DeserializeOwned;

use crate::pump::host::Ctx;

/// How the result line was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResultForm {
    Prefixed,
    Unprefixed,
}

/// Parse the last prefixed line, else the last standalone JSON object line.
pub(crate) fn find_marked<T: DeserializeOwned>(
    text: &str,
    prefix: &str,
) -> Option<(T, ResultForm)> {
    let prefixed = text
        .lines()
        .rev()
        .filter_map(|line| line.trim().strip_prefix(prefix))
        .find_map(|json| serde_json::from_str(json).ok());
    if let Some(value) = prefixed {
        return Some((value, ResultForm::Prefixed));
    }
    text.lines()
        .rev()
        .filter_map(standalone_object)
        .find_map(|json| serde_json::from_str(json).ok())
        .map(|value| (value, ResultForm::Unprefixed))
}

/// The line's content when it is exactly one JSON object, optionally wrapped
/// in backticks; prose around a fragment disqualifies the line.
fn standalone_object(line: &str) -> Option<&str> {
    let inner = line.trim().trim_matches('`').trim();
    (inner.starts_with('{') && inner.ends_with('}')).then_some(inner)
}

/// Log once that a receipt was accepted without its marker prefix.
pub(crate) fn note_unprefixed(ctx: &Ctx, job_id: &str, prefix: &str) {
    ctx.log(&format!(
        "receipt for job {job_id} accepted without its {prefix} prefix (worker format drift)"
    ));
}
