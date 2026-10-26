//! Field validation for action requests: the applicability matrix that
//! says which fields each canonical action may carry and which it requires,
//! plus the shape checks every field shares.
//!
//! `project` and `note` are deliberately outside the matrix: every action
//! happens against a watched project, and every action may carry a
//! free-text note. Every other field must be listed to apply, and a
//! supplied field the action does not list is surplus intent the turn is
//! told about. Blank fields were normalized to absent at parse time, so a
//! field seen here is real, nonblank intent.

use super::{ActionRequest, MAX_NOTE, Refusal};
use crate::mind::authority::Action;

/// Every field the matrix governs, in request declaration order. The order
/// decides which surplus field a refusal names first.
const FIELDS: &[&str] = &[
    "need",
    "step",
    "word",
    "entry",
    "file",
    "filter",
    "flow",
    "pool",
    "to",
    "subject",
    "body",
    "target",
    "role",
    "model",
    "band",
    "reasoning",
    "done_when",
    "write_scope",
    "priority",
    "cap",
    "limit",
    "lines",
    "offset",
];

/// Check one request's fields: note length, handle shape, applicability,
/// and required fields.
///
/// # Errors
/// Returns the first refusal: a note over its cap, a non-plain handle, a
/// field the action does not accept, or a required field left out.
pub fn validate_fields(request: &ActionRequest) -> Result<(), Refusal> {
    check_note(request)?;
    check_handles(request)?;
    let (allowed, required) = contract(request.kind);
    for field in FIELDS {
        if !allowed.contains(field) && carries(request, field) {
            return Err(Refusal::UnexpectedField(field));
        }
    }
    for field in required {
        if !carries(request, field) {
            return Err(Refusal::MissingField(field));
        }
    }
    Ok(())
}

/// The applicability matrix: per canonical action, the fields it may carry
/// and, of those, the fields that must be present. An absent causal `need`
/// stays the gate's `MissingNeed` refusal, so `need` is required only where
/// it is the read's handle.
fn contract(kind: Action) -> (&'static [&'static str], &'static [&'static str]) {
    match kind {
        Action::ReadStatus => (&[], &[]),
        Action::ReadPumpLog => (&["file", "lines", "filter"], &[]),
        Action::ReadPumpHistory => (&["need", "flow"], &[]),
        Action::ReadNeed => (&["need", "limit", "offset"], &["need"]),
        Action::ReadNeedThread => (&["need", "limit", "offset"], &["need"]),
        Action::ReadNeedUnits => (&["need", "limit", "offset"], &["need"]),
        Action::ReadTaskThread => (&["step", "limit", "offset"], &["step"]),
        Action::ReadConfig => (&[], &[]),
        Action::ReadPool => (&["pool"], &[]),
        // Letters take an optional causal need and nothing else.
        Action::LetterToOperator | Action::Escalate => (&["need"], &[]),
        Action::PausePump | Action::ResumePump => (&[], &[]),
        Action::Requeue => (&["need", "step"], &["step"]),
        Action::Replan => (&["need"], &[]),
        Action::Answer => (&["need", "step", "word"], &["step", "word"]),
        // Unlock alone among the pump controls takes an entry.
        Action::Unlock => (&["need", "entry"], &[]),
        Action::ReplyNeed => (&["need", "subject", "body"], &[]),
        Action::FileNeed | Action::FileFixNeed => (
            &[
                "need",
                "subject",
                "body",
                "done_when",
                "write_scope",
                "priority",
            ],
            &[],
        ),
        Action::DispatchNeed => (&["need", "entry"], &[]),
        Action::SetPriority => (&["need", "priority"], &["priority"]),
        Action::WithdrawNeed => (&["need"], &[]),
        Action::SetRoleModel => (&["need", "role", "model"], &["role", "model"]),
        Action::SetPoolCap => (&["need", "pool", "cap"], &["pool", "cap"]),
        Action::SetPoolBand => (&["need", "pool", "band"], &["pool", "band"]),
        Action::Upgrade => (&["target"], &["target"]),
        Action::SelfRelease => (&["target"], &[]),
    }
}

/// Whether the request carries a value for `field`. Unknown names have no
/// column and read as absent.
fn carries(request: &ActionRequest, field: &str) -> bool {
    match field {
        "need" => request.need.is_some(),
        "step" => request.step.is_some(),
        "word" => request.word.is_some(),
        "entry" => request.entry.is_some(),
        "file" => request.file.is_some(),
        "filter" => request.filter.is_some(),
        "flow" => request.flow.is_some(),
        "pool" => request.pool.is_some(),
        "to" => request.to.is_some(),
        "subject" => request.subject.is_some(),
        "body" => request.body.is_some(),
        "target" => request.target.is_some(),
        "role" => request.role.is_some(),
        "model" => request.model.is_some(),
        "band" => request.band.is_some(),
        "reasoning" => request.reasoning.is_some(),
        "done_when" => request.done_when.is_some(),
        "write_scope" => request.write_scope.is_some(),
        "priority" => request.priority.is_some(),
        "cap" => request.cap.is_some(),
        "limit" => request.limit.is_some(),
        "lines" => request.lines.is_some(),
        "offset" => request.offset.is_some(),
        _ => false,
    }
}

fn check_note(request: &ActionRequest) -> Result<(), Refusal> {
    if request
        .note
        .as_deref()
        .is_some_and(|note| note.chars().count() > MAX_NOTE)
    {
        return Err(Refusal::NoteTooLong);
    }
    Ok(())
}

fn check_handles(request: &ActionRequest) -> Result<(), Refusal> {
    for (field, value) in [
        ("need", request.need.as_deref()),
        ("step", request.step.as_deref()),
        ("word", request.word.as_deref()),
    ] {
        if value.is_some_and(|value| !plain_handle(value)) {
            return Err(Refusal::BadHandle(field));
        }
    }
    Ok(())
}

/// Handles and answer words are short ASCII tokens; anything else (spaces,
/// shell metacharacters, flags) is refused before it nears a command line.
fn plain_handle(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.starts_with('-')
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}
