//! The compose gate: a need, want or task body with front matter is parsed
//! and validated at send, so an invalid letter is refused and nothing is
//! stored.

use super::super::Mailspace;
use super::super::serves::canonical_wants;
use crate::error::VivariumError;
use crate::letter::sugar::opens_with_fence;
use crate::letter::{Front, Letter, LetterError, LetterKind, of_message, parse_kind, render};

/// A validated work letter body and what it declares.
pub(super) struct WorkLetter {
    /// The body to store (re-rendered when a need's `serves`/`explores` were made canonical).
    pub body: String,
    /// The front matter's `depends_on`.
    pub depends_on: Vec<String>,
    /// The parsed letter, with a need's `serves` and `explores` canonical.
    pub letter: Letter,
}

/// Parse and validate `body` when `kind` is need, want or task. Any other
/// kind returns `Ok(None)`.
///
/// # Errors
/// Returns the `LetterError` text when the front matter is invalid, or the
/// kind's refusal text when the body does not open with front matter.
pub(super) fn work_letter(
    mailspace: &Mailspace,
    kind: Option<&str>,
    body: &str,
) -> Result<Option<WorkLetter>, VivariumError> {
    let letter_kind = match kind {
        Some("need") => LetterKind::Need,
        Some("want") => LetterKind::Want,
        Some("task") => LetterKind::Task,
        _ => return Ok(None),
    };
    if !opens_with_fence(body) {
        return Err(VivariumError::Message(no_front(&letter_kind).into()));
    }
    let mut letter = parse(&letter_kind, body)?;
    let mut body = body.to_string();
    let depends_on = deps(&letter.front)?;
    let mut merged = false;
    if let Front::Need(front) = &mut letter.front
        && (front.serves.is_some() || front.explores.is_some())
    {
        let storage = mailspace.storage()?;
        canonical_wants(&storage, front)?;
        merged = true;
    }
    if merged {
        body = rerender(&letter)?;
    }
    Ok(Some(WorkLetter {
        body,
        depends_on,
        letter,
    }))
}

/// Check a raw imported letter. A letter with no `X-Vivi-Format` header is
/// history and imports as is; one that claims a format must parse.
///
/// # Errors
/// Returns the `LetterError` text when the letter claims format 1 (or an
/// unknown format) and fails the parser.
pub(super) fn raw_letter(data: &[u8]) -> Result<(), VivariumError> {
    match of_message(data, "") {
        Ok(_) | Err(LetterError::Legacy { .. }) => Ok(()),
        Err(error) => Err(VivariumError::Message(format!("letter format: {error}"))),
    }
}

fn no_front(kind: &LetterKind) -> &'static str {
    match kind {
        LetterKind::Need => {
            "need has no done_when: pass --done-when or add done_when = [...] to the front matter"
        }
        LetterKind::Want => {
            "want has no question: pass --question or add question = \"...\" to the front matter"
        }
        _ => {
            "task letter has no front matter: open the body with +++ ... +++ (an empty front matter is fine)"
        }
    }
}

fn parse(kind: &LetterKind, body: &str) -> Result<Letter, VivariumError> {
    parse_kind(kind, body).map_err(|err| VivariumError::Message(err.to_string()))
}

fn deps(front: &Front) -> Result<Vec<String>, VivariumError> {
    match front {
        Front::Need(front) => Ok(front.depends_on.clone()),
        Front::Want(front) => Ok(front.depends_on.clone()),
        Front::Task(front) => Ok(front.depends_on.clone()),
        _ => Err(VivariumError::Message("not a work letter".into())),
    }
}

fn rerender(letter: &Letter) -> Result<String, VivariumError> {
    let rendered = match &letter.front {
        Front::Need(front) => render(front, &letter.prose),
        Front::Want(front) => render(front, &letter.prose),
        Front::Task(front) => render(front, &letter.prose),
        _ => return Err(VivariumError::Message("not a work letter".into())),
    };
    rendered.map_err(|err| VivariumError::Message(err.to_string()))
}

#[cfg(test)]
#[path = "compose_test.rs"]
mod tests;
