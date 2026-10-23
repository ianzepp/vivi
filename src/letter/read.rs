//! The one entry point readers call to get the typed front of a stored need,
//! want or task letter body.

use crate::letter::records::RecordFront;
use crate::letter::work::{NeedFront, StepFront, TaskFront, WantFront};
use crate::letter::{Envelope, FENCE, LetterError, parse_letter};

/// True when the first non-blank line of `body` is the front matter fence.
fn has_front(body: &str) -> bool {
    body.lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.trim() == FENCE)
}

/// The typed front of a need body.
///
/// # Errors
///
/// Returns the `LetterError` when the front matter does not parse or validate,
/// and `LetterError::Legacy` naming `handle` for a body with no front matter.
/// `handle` names the letter in errors.
pub fn need_of(handle: &str, body: &str) -> Result<Envelope<NeedFront>, LetterError> {
    if has_front(body) {
        return parse_letter(body);
    }
    Err(LetterError::Legacy {
        handle: handle.to_string(),
        kind: "need".to_string(),
    })
}

/// The typed front of a want body.
///
/// # Errors
///
/// Returns the `LetterError` when the front matter does not parse or validate,
/// and `LetterError::Legacy` naming `handle` for a body with no front matter.
pub fn want_of(handle: &str, body: &str) -> Result<Envelope<WantFront>, LetterError> {
    if has_front(body) {
        return parse_letter(body);
    }
    Err(LetterError::Legacy {
        handle: handle.to_string(),
        kind: "want".to_string(),
    })
}

/// The typed front of a task body.
///
/// # Errors
///
/// Returns the `LetterError` when the front matter does not parse or validate,
/// and `LetterError::Legacy` naming `handle` for a body with no front matter.
pub fn task_of(handle: &str, body: &str) -> Result<Envelope<TaskFront>, LetterError> {
    if has_front(body) {
        return parse_letter(body);
    }
    Err(LetterError::Legacy {
        handle: handle.to_string(),
        kind: "task".to_string(),
    })
}

/// The typed front of a pump record body.
///
/// # Errors
///
/// Returns the `LetterError` when the front matter does not parse or validate,
/// and `LetterError::Legacy` naming `handle` for a body with no front matter.
pub fn record_of(handle: &str, body: &str) -> Result<Envelope<RecordFront>, LetterError> {
    if has_front(body) {
        return parse_letter(body);
    }
    Err(LetterError::Legacy {
        handle: handle.to_string(),
        kind: "pump".to_string(),
    })
}

/// The `[step]` table of a task body; `None` when the body is not a task
/// letter or has no step.
#[must_use]
pub fn step_of(body: &str) -> Option<StepFront> {
    if has_front(body) {
        return match parse_letter::<TaskFront>(body) {
            Ok(envelope) => envelope.front.step,
            Err(_) => None,
        };
    }
    None
}

#[cfg(test)]
#[path = "read_test.rs"]
mod tests;
