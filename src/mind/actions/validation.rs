//! Field validation for action requests: which fields each canonical kind
//! accepts, which it cannot go without, and the note and handle checks.
//!
//! The matrix is explicit. `project` and `note` are common to every action;
//! any other field must be listed for the kind or the request is refused
//! with [`Refusal::UnexpectedField`]. Required action-specific fields are
//! refused with [`Refusal::MissingField`]. A need-scoped control never has
//! `need` required here: an absent causal need stays with
//! `authority::permit`, which reports `Denial::MissingNeed`.
//!
//! Every field on [`ActionRequest`] beyond the common two must appear in
//! [`FIELDS`], or a supplied value would slip past the applicability check.

use crate::mind::authority::Action;

use super::{ActionRequest, MAX_NOTE, Refusal};

/// Every field a request may carry beyond the common `project` and `note`,
/// in canonical order. Keep in step with [`ActionRequest`].
const FIELDS: [&str; 23] = [
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

/// Check one request's fields: note length, field applicability, handle
/// shape, and required-field presence, in that order.
pub(super) fn validate_fields(request: &ActionRequest) -> Result<(), Refusal> {
    check_note(request)?;
    check_applicability(request)?;
    check_handles(request)?;
    check_required(request)
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

fn check_applicability(request: &ActionRequest) -> Result<(), Refusal> {
    let accepted = accepted_fields(request.kind);
    let supplied = FIELDS
        .into_iter()
        .find(|field| !accepted.contains(field) && is_present(request, field));
    match supplied {
        Some(field) => Err(Refusal::UnexpectedField(field)),
        None => Ok(()),
    }
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

fn check_required(request: &ActionRequest) -> Result<(), Refusal> {
    if request.project.is_none() {
        return Err(Refusal::MissingField("project"));
    }
    for field in required_fields(request.kind) {
        if !is_present(request, field) {
            return Err(Refusal::MissingField(field));
        }
    }
    Ok(())
}

/// The fields `kind` accepts beyond the common `project` and `note`.
/// Reads take only their documented arguments; letters keep their
/// destination lines; writes take their named operational, filing, or
/// tuning fields.
fn accepted_fields(kind: Action) -> &'static [&'static str] {
    match kind {
        Action::ReadStatus | Action::ReadConfig => &[],
        Action::ReadPumpLog => &["file", "lines", "filter"],
        Action::ReadPumpHistory => &["need", "flow"],
        Action::ReadNeed | Action::ReadNeedThread | Action::ReadNeedUnits => {
            &["need", "limit", "offset"]
        }
        Action::ReadTaskThread => &["step", "limit", "offset"],
        Action::ReadPool => &["pool"],
        Action::LetterToOperator | Action::Escalate => &["need", "to", "subject", "body"],
        Action::PausePump | Action::ResumePump | Action::SelfRelease => &[],
        Action::Requeue => &["need", "step"],
        Action::Replan => &["need"],
        Action::Answer => &["need", "step", "word"],
        Action::Unlock | Action::DispatchNeed => &["need", "entry"],
        Action::ReplyNeed => &["need", "body"],
        Action::FileNeed => &[
            "need",
            "target",
            "subject",
            "body",
            "done_when",
            "write_scope",
            "priority",
        ],
        Action::SetPriority => &["need", "priority"],
        Action::WithdrawNeed => &["need"],
        Action::SetRoleModel => &["role", "model", "band", "reasoning"],
        Action::SetPoolCap => &["pool", "cap"],
        Action::SetPoolBand => &["pool", "band"],
        Action::FileFixNeed => &[
            "need",
            "subject",
            "body",
            "done_when",
            "write_scope",
            "priority",
        ],
        Action::Upgrade => &["target"],
    }
}

/// The accepted fields `kind` cannot go without. `project` is required of
/// every action and checked separately. Needs left to the gate stay out:
/// `authority::permit` reports them as `MissingNeed`.
fn required_fields(kind: Action) -> &'static [&'static str] {
    match kind {
        Action::Requeue => &["step"],
        Action::Answer => &["step", "word"],
        Action::ReadNeed | Action::ReadNeedThread | Action::ReadNeedUnits => &["need"],
        Action::ReadTaskThread => &["step"],
        _ => &[],
    }
}

/// Whether the request supplies `field`. Blank strings were dropped at
/// parse time, so a present optional string is nonblank; list fields count
/// as supplied whenever they are present.
fn is_present(request: &ActionRequest, field: &str) -> bool {
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
