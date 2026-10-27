//! Durable effective need scope: typed scope rulings and the write scope a
//! dispatched slice may claim.
//!
//! A need's signed front matter delegates its original write scope. When a
//! hand answers `blocked-scope`, the pump settles the request and records the
//! ruling on the need thread as a scope-ruling pump note
//! ([`NoteWord::ScopeRuling`]) sent by the pump's reply role. The note's
//! `pump.ref` names the original source job with only the envelope-correction
//! suffix stripped ([`correct::base_id`]); its prose carries one strict TOML
//! [`ScopeRuling`] record.
//!
//! The readers here are the shared permission service for role and hand
//! collectors: the effective scope is the original explicit delegation
//! unioned with the accepted grants, deduplicated, oldest note first. Plain
//! prose, malformed records, foreign notes, records for another need or
//! target, and operator-required decisions never grant permission. Historical
//! grants are durable facts and are not rechecked against today's allow/deny
//! policy; the original signed need is never rewritten. An empty original
//! write scope grants nothing: grants widen an existing delegation, they are
//! not one.
//!
//! Adding to a scope goes through [`ScopeRequest`] and [`classify`]: the
//! request's target is resolved against the configured targets, the need's
//! explicit target and its branch row, and the paths the request does not
//! already hold are classified atomically against the target's live policy
//! and its own checkout. The authorization operation collectors call is
//! [`authorize_request`]: it classifies the whole request, journals it under
//! its source, and publishes or adopts the matching need-thread ruling note
//! ([`NoteWord::ScopeRuling`]) before any scope is mutated, so permission
//! evidence exists before a grant takes effect. [`effective_scopes`] renders
//! the reader's facts grouped per target for runtime briefs. After a
//! published or adopted allowed ruling, [`continue_role`] regenerates the
//! asking role's job at the same attempt through its registered handler, so
//! the next dispatch reads the widened authoritative scope.

use serde::{Deserialize, Serialize};

use crate::error::VivariumError;
use crate::letter::Validate;
use crate::letter::check_list;
use crate::letter::check_text;
use crate::letter::read::need_of;
use crate::letter::records::NoteWord;
use crate::mailspace::pump_note::note_body;
use crate::pump::config::ScopePath;
use crate::pump::envelope::correct;
use crate::pump::flow::slice_dispatch::SliceMail;

mod continuation;
mod publish;
mod request;

// The slice collector calls this after a published or adopted allowed
// ruling; the call site lands with that slice.
#[allow(unused_imports)]
pub(crate) use continuation::continue_role;
pub use publish::{ScopeAsk, ScopePublication, authorize_request};
pub use request::{
    RequestTarget, ScopeClassification, ScopeContext, ScopeRequest, TargetScope, classify,
    effective_scopes,
};

/// What a scope ruling decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RulingDecision {
    /// The requested paths are granted: the slice may write them.
    Grant,
    /// An operator must decide; the ruling grants nothing.
    OperatorRequired,
}

impl RulingDecision {
    /// Whether the ruling grants the paths it names.
    #[must_use]
    pub fn is_grant(self) -> bool {
        matches!(self, Self::Grant)
    }
}

/// Why a request needed an operator. One typed issue per operator-required
/// ruling, the strongest blocking reason of the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScopeIssue {
    /// The request named a target the pump does not configure.
    UnknownTarget,
    /// The request's target disagrees with the need's own target.
    TargetMismatch,
    /// A requested path is not a valid literal path.
    InvalidPath,
    /// A requested path touches a denied path, or a requested directory
    /// covers a denied descendant.
    Denied,
    /// A requested path sits outside the target's allow list.
    OutsideAllow,
    /// Deciding needs the filesystem; fails safe until verification lands.
    UnverifiablePath,
}

/// One scope ruling: the strict TOML record in the prose of a scope-ruling
/// pump note on the need thread. Paths are normalized literal scope paths
/// ([`ScopePath`]); a grant carries no issue, an operator-required decision
/// always names one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeRuling {
    /// The need the ruling belongs to.
    pub need: String,
    /// The pump target the decided paths live in.
    pub target: String,
    /// The run that asked.
    pub run: u32,
    /// The task handle of the requesting slice.
    pub task: String,
    /// The normalized paths the ruling decides about.
    pub paths: Vec<String>,
    /// One-line human reason, plain prose.
    pub reason: String,
    /// The typed decision.
    pub decision: RulingDecision,
    /// Why an operator was needed; a grant carries none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<ScopeIssue>,
}

impl Validate for ScopeRuling {
    fn validate(&self) -> Result<(), String> {
        for (key, value) in [
            ("need", &self.need),
            ("target", &self.target),
            ("task", &self.task),
            ("reason", &self.reason),
        ] {
            check_text(key, value)?;
        }
        if self.run == 0 {
            return Err("`run` must be greater than zero".to_string());
        }
        check_list("paths", &self.paths)?;
        if self.paths.is_empty() {
            return Err(
                "`paths` must not be empty: a ruling names the paths it decides about".to_string(),
            );
        }
        for path in &self.paths {
            let normalized =
                ScopePath::parse(path).map_err(|err| format!("`paths` entry `{path}`: {err}"))?;
            if normalized.as_str() != path.trim() {
                return Err(format!(
                    "`paths` entry `{path}` is not normalized: write `{}`",
                    normalized.as_str()
                ));
            }
        }
        match (self.decision, &self.issue) {
            (RulingDecision::Grant, Some(_)) => Err(
                "a grant carries no `issue`: only an operator-required decision does".to_string(),
            ),
            (RulingDecision::OperatorRequired, None) => {
                Err("an operator-required decision carries an `issue` naming why".to_string())
            }
            _ => Ok(()),
        }
    }
}

/// Encode `ruling` as the strict TOML record a scope-ruling note carries.
///
/// # Errors
/// Returns [`VivariumError`] when the record does not validate or does not
/// serialize.
pub fn encode(ruling: &ScopeRuling) -> Result<String, VivariumError> {
    ruling
        .validate()
        .map_err(|err| VivariumError::Parse(format!("scope ruling: {err}")))?;
    toml::to_string(ruling).map_err(|err| VivariumError::Parse(format!("scope ruling: {err}")))
}

/// Decode and validate the TOML record `text` carries.
///
/// # Errors
/// Returns [`VivariumError`] when the text is not a valid scope ruling
/// record.
pub fn decode(text: &str) -> Result<ScopeRuling, VivariumError> {
    let ruling: ScopeRuling =
        toml::from_str(text).map_err(|err| VivariumError::Parse(format!("scope ruling: {err}")))?;
    ruling
        .validate()
        .map_err(|err| VivariumError::Parse(format!("scope ruling: {err}")))?;
    Ok(ruling)
}

/// The body of the scope-ruling note for `ruling`: the record as TOML prose,
/// the note's `ref` naming the original source job with only the
/// envelope-correction suffix stripped.
///
/// # Errors
/// Returns [`VivariumError`] when the record does not encode or the note does
/// not render.
pub fn ruling_note_body(ruling: &ScopeRuling, source_job: &str) -> Result<String, VivariumError> {
    let record = encode(ruling)?;
    note_body(
        NoteWord::ScopeRuling,
        Some(correct::base_id(source_job)),
        &record,
    )
}

/// The effective write scope of `need`: the original explicit delegation of
/// its signed front unioned with every valid scope-ruling grant recorded on
/// the need thread by the reply role, deduplicated, original paths first.
/// Grants for another target than the need's own explicit target never apply.
///
/// # Errors
/// Returns [`VivariumError`] when the need letter cannot be read.
pub fn effective_need_scope(
    mail: &dyn SliceMail,
    need: &str,
    reply_from: &str,
) -> Result<Vec<String>, VivariumError> {
    let (original, target) = original_scope(mail, need)?;
    if original.is_empty() {
        return Ok(Vec::new());
    }
    let grants = accepted_grants(mail, need, target.as_deref(), reply_from)?;
    Ok(union(&original, &grants))
}

/// The effective write scope of `need` for one target: the original paths
/// only when the need names no other target, plus the grants recorded for
/// this target. An explicit target on the need is respected: its paths never
/// leak to another repository.
///
/// # Errors
/// Returns [`VivariumError`] when the need letter cannot be read.
pub fn effective_target_scope(
    mail: &dyn SliceMail,
    need: &str,
    target: &str,
    reply_from: &str,
) -> Result<Vec<String>, VivariumError> {
    let (original, need_target) = original_scope(mail, need)?;
    if original.is_empty() || need_target.as_deref().is_some_and(|named| named != target) {
        return Ok(Vec::new());
    }
    let grants = accepted_grants(mail, need, Some(target), reply_from)?;
    Ok(union(&original, &grants))
}

/// The original explicit delegation of `need`: its write scope and the
/// target it names.
///
/// # Errors
/// Returns [`VivariumError`] when the need letter cannot be read or does not
/// parse.
fn original_scope(
    mail: &dyn SliceMail,
    need: &str,
) -> Result<(Vec<String>, Option<String>), VivariumError> {
    let body = mail.task_body(need)?;
    let envelope = need_of(need, &body).map_err(|err| VivariumError::Parse(err.to_string()))?;
    Ok((envelope.front.write_scope, envelope.front.target))
}

/// The valid grant rulings on `need`: every scope-ruling note `reply_from`
/// sent whose `ref` names a source job, whose record decodes and validates,
/// and that grants the paths of a matching need and target. Everything else —
/// plain prose, malformed records, another need or target, an
/// operator-required decision — grants nothing and is skipped, never an
/// error.
///
/// # Errors
/// Returns [`VivariumError`] when the notes cannot be read.
fn accepted_grants(
    mail: &dyn SliceMail,
    need: &str,
    target: Option<&str>,
    reply_from: &str,
) -> Result<Vec<ScopeRuling>, VivariumError> {
    let mut grants = Vec::new();
    for (_, note) in mail.notes_from(need, NoteWord::ScopeRuling, reply_from)? {
        let named = note
            .pump
            .reference
            .as_deref()
            .map(str::trim)
            .filter(|reference| !reference.is_empty());
        if named.is_none() {
            continue;
        }
        let Ok(ruling) = decode(note.prose.trim()) else {
            continue;
        };
        if ruling.need != need || !ruling.decision.is_grant() {
            continue;
        }
        if target.is_some_and(|target| ruling.target != target) {
            continue;
        }
        grants.push(ruling);
    }
    Ok(grants)
}

/// The original paths then every grant's paths, deduplicated, first seen wins.
fn union(original: &[String], grants: &[ScopeRuling]) -> Vec<String> {
    let mut paths = Vec::new();
    for path in original {
        push_unique(&mut paths, path);
    }
    for ruling in grants {
        for path in &ruling.paths {
            push_unique(&mut paths, path);
        }
    }
    paths
}

fn push_unique(paths: &mut Vec<String>, path: &str) {
    if !paths.iter().any(|seen| seen == path) {
        paths.push(path.to_owned());
    }
}

#[cfg(test)]
#[path = "need_scope_test.rs"]
mod tests;
