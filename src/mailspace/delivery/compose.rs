//! The compose gate: every send and reply is checked at compose. The format
//! rules are the public `letter::gate`; the storage step here canonicalizes a
//! need's `serves`/`explores` against the mailspace and re-renders the body
//! when that changed. An invalid letter is refused and nothing is stored.

use super::super::Mailspace;
use super::super::serves::canonical_wants;
use crate::error::VivariumError;
use crate::letter::gate as letter_gate;
use crate::letter::{Front, Letter, LetterError, of_message, render};

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
/// `role`, else mail. The format step is the storage-free `letter::gate`; the
/// storage step canonicalizes a need's `serves`/`explores` against the
/// mailspace and re-renders the body when that changed. Memo, rule and any
/// other kind pass unchanged and unformatted.
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
    let mut gated =
        letter_gate::gate(word, body).map_err(|error| VivariumError::Message(error.to_string()))?;
    let mut body = body.to_string();
    if let Some(letter) = gated.letter.as_mut()
        && let Front::Need(front) = &mut letter.front
        && (front.serves.is_some() || front.explores.is_some())
    {
        let storage = mailspace.storage()?;
        canonical_wants(&storage, front)?;
        body = rerender(letter)?;
    }
    Ok(Gated {
        body,
        kind: Some(gated.kind),
        formatted: gated.formatted,
        depends_on: gated.depends_on,
        letter: gated.letter,
    })
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

fn rerender(letter: &Letter) -> Result<String, VivariumError> {
    let Front::Need(front) = &letter.front else {
        return Err(VivariumError::Message("not a work letter".into()));
    };
    render(front, &letter.prose).map_err(|err| VivariumError::Message(err.to_string()))
}

#[cfg(test)]
#[path = "compose_test.rs"]
mod tests;
