//! The compose gate: every send and reply is checked at compose. A need, want
//! or task body must open with valid front matter; a pump record must parse as
//! one; prose mail and chat are stored as written, with no front matter; a mail
//! body with front matter must be a note, an action or a role reply envelope. An invalid
//! letter is refused and nothing is stored.

use super::super::Mailspace;
use super::super::serves::canonical_wants;
use crate::error::VivariumError;
use crate::letter::sugar::opens_with_fence;
use crate::letter::{Front, Letter, LetterError, LetterKind, of_message, parse_kind, render};

/// A checked letter body and what it declares.
pub(super) struct Gated {
    /// The body to store: re-rendered when a
    /// need's `serves`/`explores` were made canonical, else as written.
    pub body: String,
    /// The `X-Vivi-Kind` to write.
    pub kind: Option<String>,
    /// Whether the letter carries `X-Vivi-Format: 1`.
    pub formatted: bool,
    /// The front matter's `depends_on` (need, want and task only).
    pub depends_on: Vec<String>,
    /// The parsed letter, with a need's `serves` and `explores` canonical.
    pub letter: Option<Letter>,
}

/// Check `body` for its kind: the request `kind`, else the kind of the
/// `role`, else mail. Memo, rule and any other kind pass unchanged and
/// unformatted.
///
/// # Errors
/// Returns the `LetterError` text when the front matter is invalid, or the
/// kind's refusal text when the body does not open with front matter.
pub(super) fn gate(
    mailspace: &Mailspace,
    kind: Option<&str>,
    role: &str,
    body: &str,
) -> Result<Gated, VivariumError> {
    let word = kind.unwrap_or_else(|| role_kind(role));
    match word {
        "need" | "want" | "task" => work(mailspace, word, body),
        "pump" => checked(word, body, parse(&LetterKind::Record, body)?),
        "mail" => checked_prose(word, body, &LetterKind::Mail),
        "chat" => checked_prose(word, body, &LetterKind::Chat),
        _ => Ok(Gated {
            body: body.to_string(),
            kind: Some(word.to_string()),
            formatted: false,
            depends_on: Vec::new(),
            letter: None,
        }),
    }
}

/// The kind a role's letters carry when the request names none.
fn role_kind(role: &str) -> &'static str {
    match role {
        "tasks" => "task",
        "needs" => "need",
        "wants" => "want",
        "pumps" => "pump",
        _ => "mail",
    }
}

fn checked(kind: &str, body: &str, letter: Letter) -> Result<Gated, VivariumError> {
    Ok(Gated {
        body: body.to_string(),
        kind: Some(kind.to_string()),
        formatted: true,
        depends_on: Vec::new(),
        letter: Some(letter),
    })
}

/// A body that opens with front matter must parse as `letter_kind`; any other
/// body is prose and is stored as written (it reads as an empty front plus the
/// whole body).
fn checked_prose(kind: &str, body: &str, letter_kind: &LetterKind) -> Result<Gated, VivariumError> {
    checked(kind, body, parse(letter_kind, body)?)
}

fn work(mailspace: &Mailspace, kind: &str, body: &str) -> Result<Gated, VivariumError> {
    let letter_kind = match kind {
        "need" => LetterKind::Need,
        "want" => LetterKind::Want,
        _ => LetterKind::Task,
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
    Ok(Gated {
        body,
        kind: Some(kind.to_string()),
        formatted: true,
        depends_on,
        letter: Some(letter),
    })
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
