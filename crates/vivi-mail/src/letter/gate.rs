//! The storage-free letter gate: what a body of `kind` must look like and
//! what the writer puts on the letter. This is the format half of the
//! mailspace compose gate, shared with callers that have no `Mailspace`
//! (the server's durable projects); the storage half stays in
//! `mailspace::delivery::compose`.

use crate::letter::sugar::opens_with_fence;
use crate::letter::{Front, Letter, LetterError, LetterKind, parse_kind};

/// A checked letter body: what the writer puts on the letter and what the
/// body parsed as.
#[derive(Debug, Clone, PartialEq)]
pub struct Gated {
    /// The `X-Vivi-Kind` value to write.
    pub kind: String,
    /// Whether the letter carries `X-Vivi-Format: 1`.
    pub formatted: bool,
    /// The front matter's `depends_on` (need, want and task only).
    pub depends_on: Vec<String>,
    /// The parsed letter, when the kind parses one.
    pub letter: Option<Letter>,
}

/// Check `body` for its kind: need, want and task must open with valid front
/// matter; a pump record must parse as one; prose mail, chat, memo and rule
/// are stored as written, while a body that opens with front matter must
/// parse (note, action and role reply envelope rules included); any other
/// kind passes unchanged and unformatted.
///
/// # Errors
/// Returns the `LetterError` naming the fault when the front matter is
/// invalid, or the kind's refusal text when the body does not open with
/// front matter.
pub fn gate(kind: &str, body: &str) -> Result<Gated, LetterError> {
    match kind {
        "need" | "want" | "task" => work(kind, body),
        "pump" => checked(kind, parse_kind(&LetterKind::Record, body)?),
        "mail" => prose(kind, body, &LetterKind::Mail),
        "chat" => prose(kind, body, &LetterKind::Chat),
        "memo" => prose(kind, body, &LetterKind::Memo),
        "rule" => prose(kind, body, &LetterKind::Rule),
        _ => Ok(Gated {
            kind: kind.to_string(),
            formatted: false,
            depends_on: Vec::new(),
            letter: None,
        }),
    }
}

/// A work letter: the body must open with front matter and parse as its kind.
fn work(kind: &str, body: &str) -> Result<Gated, LetterError> {
    let letter_kind = match kind {
        "need" => LetterKind::Need,
        "want" => LetterKind::Want,
        _ => LetterKind::Task,
    };
    if !opens_with_fence(body) {
        return Err(LetterError::Invalid(no_front(&letter_kind).to_string()));
    }
    let letter = parse_kind(&letter_kind, body)?;
    Ok(Gated {
        kind: kind.to_string(),
        formatted: true,
        depends_on: deps(&letter.front),
        letter: Some(letter),
    })
}

/// A body that opens with front matter must parse as `letter_kind`; any other
/// body is prose and is stored as written (it reads as an empty front plus the
/// whole body).
fn prose(kind: &str, body: &str, letter_kind: &LetterKind) -> Result<Gated, LetterError> {
    checked(kind, parse_kind(letter_kind, body)?)
}

fn checked(kind: &str, letter: Letter) -> Result<Gated, LetterError> {
    Ok(Gated {
        kind: kind.to_string(),
        formatted: true,
        depends_on: Vec::new(),
        letter: Some(letter),
    })
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

/// The front matter's `depends_on`; only work letters carry one.
fn deps(front: &Front) -> Vec<String> {
    match front {
        Front::Need(front) => front.depends_on.clone(),
        Front::Want(front) => front.depends_on.clone(),
        Front::Task(front) => front.depends_on.clone(),
        _ => Vec::new(),
    }
}
