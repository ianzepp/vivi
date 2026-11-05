//! Fence-free structured role fields, validated by the same role types as envelopes.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{
    Admitter, Auditor, Cto, Hand, HeadCeo, Judge, Merge, Parsed, Planner, Repairer, Researcher,
    RoleKind, Slicer,
};
use crate::letter::{LetterError, Validate};

/// Deserialize structured fields into the selected role and validate them.
/// The body is supplied separately; no envelope is rendered or reparsed.
pub fn parse_for(kind: RoleKind, fields: Value, body: &str) -> Result<Parsed, LetterError> {
    match kind {
        RoleKind::Researcher => parsed::<Researcher>(fields, body),
        RoleKind::Planner => parsed::<Planner>(fields, body),
        RoleKind::Admitter => parsed::<Admitter>(fields, body),
        RoleKind::Slicer => parsed::<Slicer>(fields, body),
        RoleKind::Auditor => parsed::<Auditor>(fields, body),
        RoleKind::Judge => parsed::<Judge>(fields, body),
        RoleKind::Cto => parsed::<Cto>(fields, body),
        RoleKind::Merge => parsed::<Merge>(fields, body),
        RoleKind::Hand => parsed::<Hand>(fields, body),
        RoleKind::Repairer => parsed::<Repairer>(fields, body),
        RoleKind::HeadCeo => parsed::<HeadCeo>(fields, body),
    }
}

fn parsed<T>(fields: Value, body: &str) -> Result<Parsed, LetterError>
where
    T: DeserializeOwned + Serialize + Validate,
{
    let front: T =
        serde_json::from_value(fields).map_err(|err| LetterError::Invalid(err.to_string()))?;
    front.validate().map_err(LetterError::Invalid)?;
    super::parsed_fields(front, body.trim().to_string())
}
