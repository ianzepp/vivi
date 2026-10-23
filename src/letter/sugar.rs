//! Send-time sugar: CLI flags that build a work letter's front matter.
//!
//! `need send --done-when a --target t` turns the flags into front matter and
//! the `--body` text into the prose. A body that already opens with `+++` is the
//! whole letter and takes no flag beside it.

use serde::Serialize;

use crate::letter::work::{NeedFront, TaskFront, WantFront};
use crate::letter::{FENCE, LetterError, Validate, render, split};

const NEED_NO_DONE_WHEN: &str =
    "need has no done_when: pass --done-when or add done_when = [...] to the front matter";
const WANT_NO_QUESTION: &str =
    "want has no question: pass --question or add question = \"...\" to the front matter";

/// The sugar flags of one send. Every field is empty when its flag is absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sugar {
    pub done_when: Vec<String>,
    pub target: Option<String>,
    pub write_scope: Vec<String>,
    pub serves: Option<String>,
    pub explores: Option<String>,
    pub question: Option<String>,
    pub depends_on: Vec<String>,
}

/// The body to send and the dependencies still sent beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sugared {
    pub body: String,
    pub depends_on: Vec<String>,
}

/// True when the first non-blank line of `body` is the front matter fence.
#[must_use]
pub fn opens_with_fence(body: &str) -> bool {
    body.lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        == Some(FENCE)
}

/// Apply the sugar flags of a need, task or want send to `body`.
///
/// # Errors
///
/// Returns the `LetterError` when a whole-letter body has a flag beside it,
/// when a need lacks `done_when` or a want lacks `question`, or when the
/// built front matter is invalid.
pub fn apply(kind: &str, body: &str, sugar: &Sugar) -> Result<Sugared, LetterError> {
    if opens_with_fence(body) {
        whole_letter(kind, body, sugar)
    } else if sugar.has_front_flags() {
        built(kind, body, sugar)
    } else {
        // TRANSITIONAL (W3-B2): a prose body sent as today; W3B2-37 and
        // W3B2-38 remove this branch.
        Ok(Sugared {
            body: body.to_string(),
            depends_on: sugar.depends_on.clone(),
        })
    }
}

impl Sugar {
    fn has_front_flags(&self) -> bool {
        !self.done_when.is_empty()
            || self.target.is_some()
            || !self.write_scope.is_empty()
            || self.serves.is_some()
            || self.explores.is_some()
            || self.question.is_some()
    }

    /// The first flag given, as (front matter key, flag name).
    fn first_flag(&self) -> Option<(&'static str, &'static str)> {
        let given = [
            (!self.done_when.is_empty(), "done_when", "done-when"),
            (self.target.is_some(), "target", "target"),
            (!self.write_scope.is_empty(), "write_scope", "write-scope"),
            (self.serves.is_some(), "serves", "serves"),
            (self.explores.is_some(), "explores", "explores"),
            (self.question.is_some(), "question", "question"),
            (!self.depends_on.is_empty(), "depends_on", "depends-on"),
        ];
        given
            .into_iter()
            .find(|(present, _, _)| *present)
            .map(|(_, key, flag)| (key, flag))
    }
}

fn whole_letter(kind: &str, body: &str, sugar: &Sugar) -> Result<Sugared, LetterError> {
    if let Some((key, flag)) = sugar.first_flag() {
        return Err(LetterError::Invalid(format!(
            "the body is a whole letter (it opens with {FENCE}): put {key} in its front matter instead of passing --{flag}"
        )));
    }
    let (front, _) = split(body)?;
    // Check the key on a plain table first so the serde "missing field" text
    // never reaches the operator.
    let table = match front.parse::<toml::Table>() {
        Ok(table) => Some(table),
        Err(_) => None,
    };
    match kind {
        "need" if !list_has_entries(table.as_ref(), "done_when") => {
            return Err(LetterError::Invalid(NEED_NO_DONE_WHEN.into()));
        }
        "want" if table.as_ref().is_some_and(|t| !t.contains_key("question")) => {
            return Err(LetterError::Invalid(WANT_NO_QUESTION.into()));
        }
        _ => {}
    }
    Ok(Sugared {
        body: body.to_string(),
        depends_on: Vec::new(),
    })
}

fn list_has_entries(table: Option<&toml::Table>, key: &str) -> bool {
    match table.and_then(|table| table.get(key)) {
        Some(toml::Value::Array(entries)) => !entries.is_empty(),
        // A table that does not parse, or a non-list value, falls through to
        // the typed parse, which names the real problem.
        Some(_) | None => table.is_none(),
    }
}

fn built(kind: &str, body: &str, sugar: &Sugar) -> Result<Sugared, LetterError> {
    let rendered = match kind {
        "need" => need_letter(body, sugar)?,
        "task" => task_letter(body, sugar)?,
        "want" => want_letter(body, sugar)?,
        other => {
            return Err(LetterError::Invalid(format!(
                "front matter flags do not apply to a {other}"
            )));
        }
    };
    Ok(Sugared {
        body: rendered,
        depends_on: Vec::new(),
    })
}

fn need_letter(body: &str, sugar: &Sugar) -> Result<String, LetterError> {
    if sugar.done_when.is_empty() {
        return Err(LetterError::Invalid(NEED_NO_DONE_WHEN.into()));
    }
    refuse_flag("need", "question", sugar.question.is_some())?;
    let front = NeedFront {
        done_when: sugar.done_when.clone(),
        target: sugar.target.clone(),
        depends_on: sugar.depends_on.clone(),
        write_scope: sugar.write_scope.clone(),
        priority: None,
        serves: sugar.serves.clone(),
        explores: sugar.explores.clone(),
    };
    finish(&front, body)
}

fn task_letter(body: &str, sugar: &Sugar) -> Result<String, LetterError> {
    refuse_flag("task", "question", sugar.question.is_some())?;
    refuse_flag("task", "serves", sugar.serves.is_some())?;
    refuse_flag("task", "explores", sugar.explores.is_some())?;
    let front = TaskFront {
        done_when: sugar.done_when.clone(),
        depends_on: sugar.depends_on.clone(),
        target: sugar.target.clone(),
        write_scope: sugar.write_scope.clone(),
        ..TaskFront::default()
    };
    finish(&front, body)
}

fn want_letter(body: &str, sugar: &Sugar) -> Result<String, LetterError> {
    let Some(question) = sugar.question.clone() else {
        return Err(LetterError::Invalid(WANT_NO_QUESTION.into()));
    };
    for (flag, present) in [
        ("done-when", !sugar.done_when.is_empty()),
        ("target", sugar.target.is_some()),
        ("write-scope", !sugar.write_scope.is_empty()),
        ("serves", sugar.serves.is_some()),
        ("explores", sugar.explores.is_some()),
    ] {
        refuse_flag("want", flag, present)?;
    }
    let front = WantFront {
        question,
        depends_on: sugar.depends_on.clone(),
    };
    finish(&front, body)
}

fn refuse_flag(kind: &str, flag: &str, present: bool) -> Result<(), LetterError> {
    if present {
        return Err(LetterError::Invalid(format!(
            "--{flag} does not apply to a {kind}"
        )));
    }
    Ok(())
}

fn finish<T: Serialize + Validate>(front: &T, body: &str) -> Result<String, LetterError> {
    front.validate().map_err(LetterError::Invalid)?;
    render(front, body)
}

#[cfg(test)]
#[path = "sugar_test.rs"]
mod tests;
