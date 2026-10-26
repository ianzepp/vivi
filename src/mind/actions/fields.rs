//! Field-level checks for [`ActionRequest`]s: blank normalization, the
//! required-field policy, and the handle token rule.

use super::{Action, ActionRequest, MAX_NOTE, Refusal};

/// A blank field is an absent one. The prompt lists every field, and a live
/// model filled the ones that did not apply with `""`, which the handle
/// check then refused. Numeric and list fields are left alone: a blank list
/// member is kept, not silently dropped.
pub(super) fn drop_blank_fields(mut request: ActionRequest) -> ActionRequest {
    for field in [
        &mut request.need,
        &mut request.step,
        &mut request.word,
        &mut request.note,
        &mut request.entry,
        &mut request.status,
        &mut request.file,
        &mut request.filter,
        &mut request.flow,
        &mut request.pool,
        &mut request.before,
        &mut request.role,
        &mut request.model,
        &mut request.band,
        &mut request.reasoning,
        &mut request.to,
        &mut request.subject,
        &mut request.body,
        &mut request.target,
    ] {
        if field
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            *field = None;
        }
    }
    if request
        .project
        .as_deref()
        .is_some_and(|project| project.as_os_str().is_empty())
    {
        request.project = None;
    }
    request
}

/// The per-kind field policy as of today: note length, handle shapes, and
/// the fields `requeue`/`answer` cannot do without. Kind-to-field matrices
/// for the newer actions land with the refusals that name them.
///
/// # Errors
/// Returns the first refusing field.
pub(super) fn validate_fields(request: &ActionRequest) -> Result<(), Refusal> {
    if request
        .note
        .as_deref()
        .is_some_and(|note| note.chars().count() > MAX_NOTE)
    {
        return Err(Refusal::NoteTooLong);
    }
    for (field, value) in [
        ("need", request.need.as_deref()),
        ("step", request.step.as_deref()),
        ("word", request.word.as_deref()),
    ] {
        if value.is_some_and(|value| !plain_handle(value)) {
            return Err(Refusal::BadHandle(field));
        }
    }
    let needs_step = matches!(request.kind, Action::Requeue | Action::Answer);
    if needs_step && request.step.is_none() {
        return Err(Refusal::MissingField("step"));
    }
    if request.kind == Action::Answer && request.word.is_none() {
        return Err(Refusal::MissingField("word"));
    }
    Ok(())
}

/// Handles and answer words are short ASCII tokens; anything else (spaces,
/// shell metacharacters, flags) is refused before it nears a command line.
pub(super) fn plain_handle(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.starts_with('-')
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}
