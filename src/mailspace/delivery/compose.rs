//! The compose gate: a need, want or task body with front matter is parsed
//! and validated at send, so an invalid letter is refused and nothing is
//! stored.

use crate::error::VivariumError;
use crate::letter::{Front, Letter, LetterKind, parse_kind, render};

/// A validated work letter body and what it declares.
pub(super) struct WorkLetter {
    /// The body to store (re-rendered when request dependencies were merged).
    pub body: String,
    /// The front matter's `depends_on`, after any merge.
    pub depends_on: Vec<String>,
    /// The parsed letter; W3B2-13 reads its front matter at attach time.
    #[allow(dead_code)]
    pub letter: Letter,
}

/// Parse and validate `body` when `kind` is need, want or task and the body
/// opens with front matter.
///
/// TRANSITIONAL (W3-B2): a non-empty request `depends_on` is merged into the
/// front matter; W3B2-37 removes it. A prose body returns `Ok(None)` and is
/// stored as before; W3B2-38 refuses it.
///
/// # Errors
/// Returns the `LetterError` text when the front matter is invalid.
pub(super) fn work_letter(
    kind: Option<&str>,
    body: &str,
    depends_on: &[String],
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
    let list = deps_mut(&mut letter.front)?;
    let mut merged = false;
    for dep in depends_on {
        if !list.contains(dep) {
            list.push(dep.clone());
            merged = true;
        }
    }
    let depends_on = list.clone();
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

fn deps_mut(front: &mut Front) -> Result<&mut Vec<String>, VivariumError> {
    match front {
        Front::Need(front) => Ok(&mut front.depends_on),
        Front::Want(front) => Ok(&mut front.depends_on),
        Front::Task(front) => Ok(&mut front.depends_on),
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
