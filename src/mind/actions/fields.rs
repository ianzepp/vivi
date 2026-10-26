//! Field-level checks for [`ActionRequest`]s: blank normalization, the
//! per-kind field matrix, the value rules, and the handle token rule.

use super::filing_fields;
use super::{Action, ActionRequest, MAX_NOTE, Refusal};
use crate::pool::config::is_model_slug;

/// Flows a read-flow may name.
const FLOWS: [&str; 4] = ["build", "research", "release", "repair"];
/// Files a read-log may name.
const FILES: [&str; 2] = ["log", "exceptions"];
/// Status filters a read-status may name.
const STATUSES: [&str; 3] = ["open", "done", "all"];

/// Every envelope field the matrix knows, in report order. `kind` and
/// `project` are handled outside the matrix.
const KNOWN: [&str; 25] = [
    "need",
    "step",
    "word",
    "note",
    "entry",
    "status",
    "limit",
    "file",
    "lines",
    "filter",
    "flow",
    "pool",
    "before",
    "priority",
    "role",
    "model",
    "cap",
    "band",
    "reasoning",
    "to",
    "subject",
    "body",
    "done_when",
    "target",
    "write_scope",
];

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

/// One field's value, as the request carries it.
enum Field<'a> {
    Absent,
    Str(&'a str),
    Int(i64),
    List(&'a [String]),
}

impl Field<'_> {
    /// Blank strings and empty lists count as absent, so a hand-built
    /// request obeys the same blank rule the parse applies.
    fn present(&self) -> bool {
        match self {
            Self::Absent => false,
            Self::Str(value) => !value.trim().is_empty(),
            Self::Int(_) => true,
            Self::List(values) => !values.is_empty(),
        }
    }
}

fn str_field(value: &Option<String>) -> Field<'_> {
    value.as_deref().map_or(Field::Absent, Field::Str)
}

/// Look a field up by its matrix name.
fn field<'a>(request: &'a ActionRequest, name: &str) -> Field<'a> {
    match name {
        "need" => str_field(&request.need),
        "step" => str_field(&request.step),
        "word" => str_field(&request.word),
        "note" => str_field(&request.note),
        "entry" => str_field(&request.entry),
        "status" => str_field(&request.status),
        "file" => str_field(&request.file),
        "filter" => str_field(&request.filter),
        "flow" => str_field(&request.flow),
        "pool" => str_field(&request.pool),
        "before" => str_field(&request.before),
        "role" => str_field(&request.role),
        "model" => str_field(&request.model),
        "band" => str_field(&request.band),
        "reasoning" => str_field(&request.reasoning),
        "to" => str_field(&request.to),
        "subject" => str_field(&request.subject),
        "body" => str_field(&request.body),
        "target" => str_field(&request.target),
        "limit" => request.limit.map_or(Field::Absent, Field::Int),
        "lines" => request.lines.map_or(Field::Absent, Field::Int),
        "priority" => request.priority.map_or(Field::Absent, Field::Int),
        "cap" => request.cap.map_or(Field::Absent, Field::Int),
        "done_when" => Field::List(&request.done_when),
        "write_scope" => Field::List(&request.write_scope),
        _ => Field::Absent,
    }
}

/// What one kind of action cannot do without and what else it reads.
/// Every other known field is refused as inappropriate. The tuple order is
/// (required, optional).
struct Spec(&'static [&'static str], &'static [&'static str]);

fn spec(kind: Action) -> Spec {
    match kind {
        Action::ReadStatus => Spec(&[], &["need", "status", "limit"]),
        Action::ReadLog => Spec(&["file"], &["lines", "filter"]),
        Action::ReadNeed => Spec(&["need"], &[]),
        Action::ReadNeedThread => Spec(&["need"], &["limit", "before"]),
        Action::ReadTaskThread => Spec(&["step"], &["limit", "before"]),
        Action::ReadFlow => Spec(&["flow"], &["need"]),
        Action::ReadPool => Spec(&["pool"], &[]),
        Action::LetterToOperator | Action::Escalate => Spec(&[], &["need", "note"]),
        Action::PausePump | Action::ResumePump => Spec(&[], &[]),
        Action::Requeue => Spec(&["need", "step"], &["note"]),
        Action::Replan => Spec(&["need"], &[]),
        Action::Answer => Spec(&["need", "step", "word"], &["note"]),
        Action::Unlock => Spec(&["need"], &["entry", "note"]),
        Action::ReplyNeed | Action::WithdrawNeed => Spec(&["need", "note"], &[]),
        Action::SetPriority => Spec(&["need", "priority"], &[]),
        Action::SetRoleModel => Spec(&["role", "model"], &[]),
        Action::SetPoolCap => Spec(&["pool", "model", "cap"], &[]),
        Action::SetPoolBand => Spec(&["pool", "band", "model", "reasoning"], &[]),
        Action::FileNeed | Action::FileFixNeed => Spec(
            &["to", "subject", "body", "done_when", "target"],
            &["write_scope"],
        ),
        Action::DispatchNeed => Spec(&["need"], &["note"]),
        Action::ReadConfig | Action::Upgrade | Action::SelfRelease => Spec(&[], &[]),
    }
}

/// Enforce the matrix for one request: every required field present, every
/// present allowed field well-formed, and no field the kind never reads.
/// Blanks were dropped at parse (and count as absent here too), so a blank
/// required note is simply missing and a blank inappropriate field is
/// never refused.
///
/// # Errors
/// Returns the first refusing field: missing before malformed before
/// unexpected.
pub(super) fn validate_fields(request: &ActionRequest) -> Result<(), Refusal> {
    let Spec(required, optional) = spec(request.kind);
    for &name in required {
        if !field(request, name).present() {
            return Err(Refusal::MissingField(name));
        }
    }
    for &name in required.iter().chain(optional.iter()) {
        check_value(name, field(request, name))?;
    }
    if matches!(request.kind, Action::FileNeed | Action::FileFixNeed) {
        filing_fields::validate_filing(request)?;
    }
    for name in KNOWN {
        if required.contains(&name) || optional.contains(&name) {
            continue;
        }
        if field(request, name).present() {
            return Err(Refusal::UnexpectedField(name));
        }
    }
    Ok(())
}

/// Check one present, allowed field against its value rule.
fn check_value(name: &'static str, value: Field<'_>) -> Result<(), Refusal> {
    match (name, value) {
        ("note", Field::Str(note)) if note.chars().count() > MAX_NOTE => Err(Refusal::NoteTooLong),
        (
            "need" | "step" | "word" | "entry" | "role" | "to" | "pool" | "target" | "before",
            Field::Str(value),
        ) if !plain_handle(value) => Err(Refusal::BadHandle(name)),
        ("model", Field::Str(value)) if !is_model_slug(value) => Err(Refusal::InvalidField(name)),
        ("flow", Field::Str(value)) if !FLOWS.contains(&value) => Err(Refusal::InvalidField(name)),
        ("file", Field::Str(value)) if !FILES.contains(&value) => Err(Refusal::InvalidField(name)),
        ("status", Field::Str(value)) if !STATUSES.contains(&value) => {
            Err(Refusal::InvalidField(name))
        }
        ("band", Field::Str(value)) if !is_band(value) => Err(Refusal::InvalidField(name)),
        ("reasoning", Field::Str(value)) if !is_reasoning(value) => {
            Err(Refusal::InvalidField(name))
        }
        ("limit", Field::Int(value)) if !(1..=100).contains(&value) => {
            Err(Refusal::InvalidField(name))
        }
        ("lines", Field::Int(value)) if !(1..=500).contains(&value) => {
            Err(Refusal::InvalidField(name))
        }
        ("priority", Field::Int(value)) if value < 0 => Err(Refusal::InvalidField(name)),
        ("cap", Field::Int(value)) if !(0..=i64::from(u32::MAX)).contains(&value) => {
            Err(Refusal::InvalidField(name))
        }
        _ => Ok(()),
    }
}

/// Compute bands are `P0` through `P5`.
fn is_band(value: &str) -> bool {
    matches!(value, "P0" | "P1" | "P2" | "P3" | "P4" | "P5")
}

/// Reasoning effort is 1-32 ASCII alphanumeric, dot, underscore or hyphen
/// characters.
fn is_reasoning(value: &str) -> bool {
    (1..=32).contains(&value.chars().count())
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
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
