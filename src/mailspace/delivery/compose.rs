//! The compose gate: a need, want or task body with front matter is parsed
//! and validated at send, so an invalid letter is refused and nothing is
//! stored.

use super::super::Mailspace;
use super::super::serves::canonical_wants;
use crate::error::VivariumError;
use crate::letter::{Front, Letter, LetterKind, parse_kind, render};

/// A validated work letter body and what it declares.
pub(super) struct WorkLetter {
    /// The body to store (re-rendered when a need's `serves`/`explores` were made canonical).
    pub body: String,
    /// The front matter's `depends_on`.
    pub depends_on: Vec<String>,
    /// The parsed letter, with a need's `serves` and `explores` canonical.
    pub letter: Letter,
}

/// Parse and validate `body` when `kind` is need, want or task and the body
/// opens with front matter.
///
/// TRANSITIONAL (W3-B2): a prose body returns `Ok(None)` and is stored as
/// before; W3B2-38 refuses it.
///
/// # Errors
/// Returns the `LetterError` text when the front matter is invalid.
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
    if body
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        != Some("+++")
    {
        return Ok(None);
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
