//! The letter kinds and their dispatch.
//!
//! A stored letter names its kind in `X-Vivi-Kind` (or `X-Vivi-Event`); the
//! kind picks the front matter schema. `parse_kind` is the only dispatch.

use serde::de::DeserializeOwned;

use crate::letter::plain::{CLOSE_EVENTS, CloseFront, EmptyFront, MemoFront, RuleFront};
use crate::letter::records::{ActionFront, MailFront, RecordFront};
use crate::letter::sugar::opens_with_fence;
use crate::letter::work::{NeedFront, TaskFront, WantFront};
use crate::letter::{LetterError, Validate, parse_letter, split};
use crate::pump::envelope::roles::{self, Parsed, RoleKind};

/// What a letter is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LetterKind {
    Need,
    Want,
    Task,
    Memo,
    Rule,
    Record,
    Action,
    /// An event letter; holds the lowercased `X-Vivi-Event` verb.
    Event(String),
    Mail,
    Chat,
    Reply(RoleKind),
}

impl LetterKind {
    /// The `X-Vivi-Kind` value a writer puts on the letter. A pool receipt
    /// (`Reply`) is not a stored letter and has none.
    #[must_use]
    pub fn header(&self) -> Option<&'static str> {
        match self {
            Self::Need => Some("need"),
            Self::Want => Some("want"),
            Self::Task => Some("task"),
            Self::Memo => Some("memo"),
            Self::Rule => Some("rule"),
            Self::Record => Some("pump"),
            Self::Action | Self::Event(_) | Self::Mail => Some("mail"),
            Self::Chat => Some("chat"),
            Self::Reply(_) => None,
        }
    }

    /// Read the kind from the `X-Vivi-Event` and `X-Vivi-Kind` headers. An
    /// event verb wins; otherwise the kind header must name one of the eight
    /// stored kinds.
    ///
    /// # Errors
    ///
    /// Returns `LetterError::UnknownKind` for a missing, blank or unknown kind.
    pub fn from_headers(kind: Option<&str>, event: Option<&str>) -> Result<Self, LetterError> {
        let verb = event.map(|value| value.trim().to_lowercase());
        if let Some(verb) = verb.filter(|verb| !verb.is_empty()) {
            return Ok(Self::Event(verb));
        }
        let word = kind.map(str::trim).unwrap_or_default();
        match word.to_lowercase().as_str() {
            "need" => Ok(Self::Need),
            "want" => Ok(Self::Want),
            "task" => Ok(Self::Task),
            "memo" => Ok(Self::Memo),
            "rule" => Ok(Self::Rule),
            "pump" => Ok(Self::Record),
            "mail" => Ok(Self::Mail),
            "chat" => Ok(Self::Chat),
            _ => Err(LetterError::UnknownKind(word.to_string())),
        }
    }
}

/// The typed front matter of a parsed letter.
#[derive(Debug, Clone, PartialEq)]
pub enum Front {
    Need(NeedFront),
    Want(WantFront),
    Task(TaskFront),
    Memo(MemoFront),
    Rule(RuleFront),
    Record(RecordFront),
    Action(ActionFront),
    Event(CloseFront),
    Mail(MailFront),
    Chat,
    Reply(Parsed),
}

/// A parsed letter: its kind, typed front matter and prose.
#[derive(Debug, Clone, PartialEq)]
pub struct Letter {
    pub kind: LetterKind,
    pub front: Front,
    pub prose: String,
}

/// Parse `text` as the letter kind `kind`.
///
/// # Errors
///
/// Returns the `LetterError` naming what to fix in the front matter.
pub fn parse_kind(kind: &LetterKind, text: &str) -> Result<Letter, LetterError> {
    match kind {
        LetterKind::Need => stored(kind, text, Front::Need),
        LetterKind::Want => stored(kind, text, Front::Want),
        LetterKind::Task => stored(kind, text, Front::Task),
        LetterKind::Memo => stored(kind, text, Front::Memo),
        LetterKind::Rule => stored(kind, text, Front::Rule),
        LetterKind::Record => stored(kind, text, Front::Record),
        LetterKind::Action => stored(kind, text, Front::Action),
        LetterKind::Event(verb) => parse_event(kind, verb, text),
        LetterKind::Mail => parse_mail(text),
        LetterKind::Chat if !opens_with_fence(text) => Ok(Letter {
            kind: kind.clone(),
            front: Front::Chat,
            prose: text.trim().to_string(),
        }),
        LetterKind::Chat => {
            let letter = parse_letter::<EmptyFront>(text)?;
            Ok(Letter {
                kind: kind.clone(),
                front: Front::Chat,
                prose: letter.body,
            })
        }
        LetterKind::Reply(role) => {
            let parsed = roles::parse_for(*role, text)?;
            Ok(Letter {
                kind: kind.clone(),
                prose: parsed.body.clone(),
                front: Front::Reply(parsed),
            })
        }
    }
}

fn stored<T>(kind: &LetterKind, text: &str, wrap: fn(T) -> Front) -> Result<Letter, LetterError>
where
    T: DeserializeOwned + Validate,
{
    let letter = parse_letter::<T>(text)?;
    Ok(Letter {
        kind: kind.clone(),
        front: wrap(letter.front),
        prose: letter.body,
    })
}

fn parse_event(kind: &LetterKind, verb: &str, text: &str) -> Result<Letter, LetterError> {
    if CLOSE_EVENTS.contains(&verb) {
        return stored(kind, text, Front::Event);
    }
    let letter = parse_letter::<EmptyFront>(text)?;
    Ok(Letter {
        kind: kind.clone(),
        front: Front::Event(CloseFront::default()),
        prose: letter.body,
    })
}

/// A mail letter with no front matter is plain prose mail with an empty front.
/// A mail letter whose front matter has a top-level `action` key is a pump
/// action; any other mail is plain mail with an optional pump note, or a role
/// reply envelope (a seat's report sent with `vivi mail reply`).
fn parse_mail(text: &str) -> Result<Letter, LetterError> {
    if !opens_with_fence(text) {
        return Ok(Letter {
            kind: LetterKind::Mail,
            front: Front::Mail(MailFront::default()),
            prose: text.trim().to_string(),
        });
    }
    if names_action(text) {
        return stored(&LetterKind::Action, text, Front::Action);
    }
    let error = match stored(&LetterKind::Mail, text, Front::Mail) {
        Ok(letter) => return Ok(letter),
        Err(error) => error,
    };
    let answer = answer_of(text);
    let mut first_fault = None;
    for role in RoleKind::ALL {
        match roles::parse_for(role, text) {
            Ok(parsed) => {
                return Ok(Letter {
                    kind: LetterKind::Reply(role),
                    prose: parsed.body.clone(),
                    front: Front::Reply(parsed),
                });
            }
            Err(fault) => {
                let named = answer
                    .as_deref()
                    .is_some_and(|word| role.answer_words().contains(&word));
                if first_fault.is_none() && named {
                    first_fault = Some(role_fault(role, fault, text));
                }
            }
        }
    }
    Err(match (first_fault, answer) {
        (Some(fault), _) => fault,
        (None, Some(word)) => LetterError::Invalid(unknown_answer(&word)),
        (None, None) => not_an_envelope(error),
    })
}

/// The value of the top-level `answer` key; `None` when there is none.
fn answer_of(text: &str) -> Option<String> {
    let Ok((front, _)) = split(text) else {
        return None;
    };
    let Ok(table) = toml::from_str::<toml::Table>(front) else {
        return None;
    };
    table.get("answer").map(|value| match value.as_str() {
        Some(word) => word.to_string(),
        None => value.to_string(),
    })
}

/// A role's own error, told which role it is. A TOML error that does not
/// name its key is told the key on its line.
fn role_fault(role: RoleKind, error: LetterError, text: &str) -> LetterError {
    let name = role.name();
    match error {
        LetterError::Toml { line, message } => {
            let key = text
                .lines()
                .nth(line.saturating_sub(1))
                .and_then(|row| row.split('=').next())
                .map(|key| key.trim().trim_matches(['[', ']']).trim())
                .filter(|key| !key.is_empty() && !message.contains(key));
            let message = match key {
                Some(key) => format!("{name} envelope: {message} (at `{key}`)"),
                None => format!("{name} envelope: {message}"),
            };
            LetterError::Toml { line, message }
        }
        LetterError::Invalid(reason) => LetterError::Invalid(format!("{name} envelope: {reason}")),
        other => other,
    }
}

/// The answer word fits no role: list what each role accepts.
fn unknown_answer(word: &str) -> String {
    let roles: Vec<String> = RoleKind::ALL
        .iter()
        .map(|role| format!("{}: {}", role.name(), role.answer_words().join(", ")))
        .collect();
    format!(
        "answer `{word}` is not a word any role accepts ({})",
        roles.join("; ")
    )
}

/// The mail error, told that the front matter is not a role envelope either.
/// A split failure is about the fence, not the keys: it is left as is.
fn not_an_envelope(error: LetterError) -> LetterError {
    const NOTE: &str = " (and the front matter is not a role reply envelope)";
    match error {
        LetterError::Toml { line, message } => LetterError::Toml {
            line,
            message: format!("{message}{NOTE}"),
        },
        LetterError::Invalid(reason) => LetterError::Invalid(format!("{reason}{NOTE}")),
        other => other,
    }
}

/// A split or TOML error is not an action: the mail parse reports it.
fn names_action(text: &str) -> bool {
    if let Ok((front, _)) = split(text)
        && let Ok(table) = toml::from_str::<toml::Table>(front)
    {
        return table.contains_key("action");
    }
    false
}

#[cfg(test)]
#[path = "kinds_test.rs"]
mod tests;
