//! The one entry point readers call to get the typed front of a stored need,
//! want or task letter body.

use crate::letter::work::{NeedFront, StepFront, TaskFront, WantFront};
use crate::letter::{Envelope, FENCE, LetterError, legacy, parse_letter};

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
/// Returns the `LetterError` when the front matter does not parse or validate.
/// `handle` names the letter in errors.
pub fn need_of(_handle: &str, body: &str) -> Result<Envelope<NeedFront>, LetterError> {
    if has_front(body) {
        return parse_letter(body);
    }
    // TRANSITIONAL (W3-B2): deleted by W3B2-39
    Ok(legacy::need(body))
}

/// The typed front of a want body.
///
/// # Errors
///
/// Returns the `LetterError` when the front matter does not parse or validate.
pub fn want_of(_handle: &str, body: &str) -> Result<Envelope<WantFront>, LetterError> {
    if has_front(body) {
        return parse_letter(body);
    }
    // TRANSITIONAL (W3-B2): deleted by W3B2-39
    Ok(legacy::want(body))
}

/// The typed front of a task body.
///
/// # Errors
///
/// Returns the `LetterError` when the front matter does not parse or validate.
pub fn task_of(_handle: &str, body: &str) -> Result<Envelope<TaskFront>, LetterError> {
    if has_front(body) {
        return parse_letter(body);
    }
    // TRANSITIONAL (W3-B2): deleted by W3B2-39
    Ok(legacy::task(body))
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
    // TRANSITIONAL (W3-B2): deleted by W3B2-39
    legacy::task(body).front.step
}

#[cfg(test)]
#[path = "read_test.rs"]
mod tests;
