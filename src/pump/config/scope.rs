//! Normalized repository-relative scope paths and target scope policy types.
//!
//! A scope path is declared relative to a target's repository root and
//! normalized without consulting the filesystem: leading `./` and
//! current-directory components drop, `.` and `./` are the repository root,
//! and a trailing slash marks a directory. Blank, absolute, and
//! parent-traversal declarations are rejected. Files are exact declarations;
//! directories cover themselves and every descendant by component boundary.
//! No globs.
//!
//! Ruling: "default the whole repository except paths it names in
//! `scope_deny`." An omitted `scope_allow` is `["./"]` (the whole
//! repository); an explicit `scope_allow = []` allows nothing; `scope_deny`
//! defaults to empty. This module supplies primitives and evidence — path
//! normalization, coverage and intersection, a serializable policy snapshot,
//! typed rejection reasons, and an all-or-nothing request authorizer over a
//! resolved target's policy. Policy eligibility is not itself a need-scope
//! grant: nothing here authorizes a dispatch on its own.

use std::collections::BTreeMap;

use super::PumpTarget;
use serde::{Deserialize, Serialize};

/// The allow default for an omitted `scope_allow`: the whole repository.
pub const WHOLE_REPOSITORY: &str = "./";

/// Why a declared scope path is not usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopePathError {
    /// The declaration is empty or whitespace.
    Blank,
    /// The declaration starts at an absolute root instead of the repository.
    Absolute,
    /// The declaration climbs out of the repository with `..`.
    ParentTraversal,
}

impl std::fmt::Display for ScopePathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blank => f.write_str("a scope path must not be blank"),
            Self::Absolute => f.write_str("a scope path must be repository-relative, not absolute"),
            Self::ParentTraversal => {
                f.write_str("a scope path must not climb out of the repository with `..`")
            }
        }
    }
}

impl std::error::Error for ScopePathError {}

/// One normalized repository-relative scope path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopePath {
    /// Path components below the repository root; empty is the root.
    segments: Vec<String>,
    /// Declared with a trailing slash (or as the root): covers descendants.
    directory: bool,
}

impl ScopePath {
    /// The repository root, covering everything.
    #[must_use]
    pub fn root() -> Self {
        Self {
            segments: Vec::new(),
            directory: true,
        }
    }

    /// Parse and normalize a declared scope path without touching the
    /// filesystem.
    ///
    /// # Errors
    /// [`ScopePathError`] when the declaration is blank, absolute, or climbs
    /// out of the repository with `..`.
    pub fn parse(raw: &str) -> Result<Self, ScopePathError> {
        let text = raw.trim();
        if text.is_empty() {
            return Err(ScopePathError::Blank);
        }
        if text.starts_with('/') {
            return Err(ScopePathError::Absolute);
        }
        let mut segments = Vec::new();
        for part in text.split('/') {
            match part {
                "" | "." => {}
                ".." => return Err(ScopePathError::ParentTraversal),
                other => segments.push(String::from(other)),
            }
        }
        Ok(Self {
            directory: text.ends_with('/') || segments.is_empty(),
            segments,
        })
    }

    /// The normalized declaration: `./` for the root, else the components
    /// joined by `/` with a trailing `/` marking a directory. Re-parsing a
    /// canonical form yields the same path.
    #[must_use]
    pub fn canonical(&self) -> String {
        if self.segments.is_empty() {
            return String::from(WHOLE_REPOSITORY);
        }
        let mut text = self.segments.join("/");
        if self.directory {
            text.push('/');
        }
        text
    }

    /// True for `.` and `./`, the declaration that covers the whole
    /// repository.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.segments.is_empty()
    }

    /// True when the declaration covers descendants, not just itself.
    #[must_use]
    pub fn is_directory(&self) -> bool {
        self.directory
    }

    /// The first path component, or none at the repository root.
    #[must_use]
    pub fn first_segment(&self) -> Option<&str> {
        self.segments.first().map(String::as_str)
    }

    /// True when this declaration covers `other`: the root covers the whole
    /// repository, a directory covers itself and every descendant by
    /// component boundary, and a file covers only its exact self.
    #[must_use]
    pub fn covers(&self, other: &Self) -> bool {
        if self.is_root() {
            return true;
        }
        if !self.directory {
            return self == other;
        }
        other.segments.starts_with(&self.segments)
    }

    /// True when the two declarations share at least one repository path.
    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        self.covers(other) || other.covers(self)
    }
}

/// True when one entry of `set` covers `path`.
#[must_use]
pub fn set_covers(set: &[ScopePath], path: &ScopePath) -> bool {
    set.iter().any(|entry| entry.covers(path))
}

/// The first pair of entries, in declaration order, that share at least one
/// repository path; none when the two sets are disjoint.
#[must_use]
pub fn first_overlap<'a>(
    left: &'a [ScopePath],
    right: &'a [ScopePath],
) -> Option<(&'a ScopePath, &'a ScopePath)> {
    left.iter()
        .find_map(|a| right.iter().find(|b| a.intersects(b)).map(|b| (a, b)))
}

/// Serializable snapshot of one target's resolved scope policy.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScopePolicy {
    /// Target id the policy belongs to.
    pub target: String,
    /// Normalized allow entries; empty allows nothing (explicit `[]`).
    pub allow: Vec<String>,
    /// Normalized deny entries; empty denies nothing. Deny wins over allow.
    pub deny: Vec<String>,
}

impl ScopePolicy {
    /// Snapshot of `target`'s policy from resolved entries, as
    /// `PumpConfig::target_configs` provides them (allow already default
    /// filled). Every entry is normalized; a blank, absolute, or
    /// parent-traversal declaration is rejected without consulting the
    /// filesystem.
    ///
    /// # Errors
    /// [`ScopeRejection::InvalidPath`] naming the first offending entry.
    pub fn snapshot(
        target: &str,
        allow: &[String],
        deny: &[String],
    ) -> Result<Self, ScopeRejection> {
        Ok(Self {
            target: String::from(target),
            allow: normalized(allow)?,
            deny: normalized(deny)?,
        })
    }

    /// Mechanical check of one declared path against this snapshot: deny
    /// wins over allow, and an empty allow allows nothing. A declaration
    /// that fails normalization comes back as
    /// [`ScopeRejection::InvalidPath`].
    #[must_use]
    pub fn check(&self, raw: &str) -> ScopeDecision {
        let path = match ScopePath::parse(raw) {
            Ok(path) => path,
            Err(error) => {
                return ScopeDecision::Reject(ScopeRejection::InvalidPath {
                    path: String::from(raw),
                    problem: error.to_string(),
                });
            }
        };
        if let Some(entry) = parsed(&self.deny).iter().find(|entry| entry.covers(&path)) {
            return ScopeDecision::Reject(ScopeRejection::DeniedPath {
                path: path.canonical(),
                deny: entry.canonical(),
            });
        }
        if set_covers(&parsed(&self.allow), &path) {
            return ScopeDecision::Allow;
        }
        ScopeDecision::Reject(ScopeRejection::OutsideAllow {
            path: path.canonical(),
            allow: self.allow.clone(),
        })
    }

    /// All-or-nothing authorization of one whole request against this
    /// policy. Every requested path must normalize — invalid input fails
    /// closed — and be eligible: any intersection between a requested path
    /// and a deny entry, in either direction, rejects the whole request, and
    /// an allow entry must cover each requested path completely, so a
    /// directory request needs one allow entry covering its descendants
    /// while a file entry covers only its exact self. One failure rejects
    /// the whole request; an allowed subset of a mixed request is never
    /// granted. Eligibility under this policy is not itself a need-scope
    /// grant.
    #[must_use]
    pub fn authorize(&self, request: &[String]) -> ScopeAuthorization {
        let mut requested = Vec::with_capacity(request.len());
        for raw in request {
            match ScopePath::parse(raw) {
                Ok(path) => requested.push(path),
                Err(error) => {
                    return ScopeAuthorization::Reject(ScopeRejection::InvalidPath {
                        path: String::from(raw),
                        problem: error.to_string(),
                    });
                }
            }
        }
        let denies = parsed(&self.deny);
        for path in &requested {
            if let Some(entry) = denies.iter().find(|entry| entry.intersects(path)) {
                return ScopeAuthorization::Reject(ScopeRejection::DeniedPath {
                    path: path.canonical(),
                    deny: entry.canonical(),
                });
            }
        }
        let allows = parsed(&self.allow);
        for path in &requested {
            if !allows.iter().any(|entry| entry.covers(path)) {
                return ScopeAuthorization::Reject(ScopeRejection::OutsideAllow {
                    path: path.canonical(),
                    allow: self.allow.clone(),
                });
            }
        }
        ScopeAuthorization::Grant(requested.iter().map(ScopePath::canonical).collect())
    }
}

/// `entries` normalized to canonical declarations; the first unusable
/// declaration fails the whole list.
fn normalized(entries: &[String]) -> Result<Vec<String>, ScopeRejection> {
    entries
        .iter()
        .map(|entry| match ScopePath::parse(entry) {
            Ok(path) => Ok(path.canonical()),
            Err(error) => Err(ScopeRejection::InvalidPath {
                path: entry.clone(),
                problem: error.to_string(),
            }),
        })
        .collect()
}

/// Entries re-parsed for coverage checks; a snapshot never carries an
/// unparsable entry, so the filter only guards direct construction.
fn parsed(entries: &[String]) -> Vec<ScopePath> {
    entries
        .iter()
        .filter_map(|entry| match ScopePath::parse(entry) {
            Ok(path) => Some(path),
            Err(_) => None,
        })
        .collect()
}

/// Why a path, target, or need was rejected against target scope policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum ScopeRejection {
    /// No allow entry covers the path.
    OutsideAllow {
        /// The checked path, normalized.
        path: String,
        /// The policy's allow entries, normalized.
        allow: Vec<String>,
    },
    /// A deny entry covers the path.
    DeniedPath {
        /// The checked path, normalized.
        path: String,
        /// The deny entry that covers `path`, normalized.
        deny: String,
    },
    /// The declaration is not a usable scope path.
    InvalidPath {
        /// The rejected declaration, as written.
        path: String,
        /// Why the declaration is unusable.
        problem: String,
    },
    /// The unit names a target the pump does not configure.
    UnknownTarget {
        /// The target name the unit carries.
        target: String,
        /// Target ids the pump configures.
        known: Vec<String>,
    },
    /// The unit and its need name different targets.
    NeedTargetMismatch {
        /// The target the unit names.
        unit_target: String,
        /// The target the need names.
        need_target: String,
    },
}

impl ScopeRejection {
    /// The machine-readable reason, matching the serialized variant tag.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::OutsideAllow { .. } => "outside_allow",
            Self::DeniedPath { .. } => "denied_path",
            Self::InvalidPath { .. } => "invalid_path",
            Self::UnknownTarget { .. } => "unknown_target",
            Self::NeedTargetMismatch { .. } => "need_target_mismatch",
        }
    }
}

impl std::fmt::Display for ScopeRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutsideAllow { path, .. } => {
                write!(f, "path `{path}` is outside the target's scope allow list")
            }
            Self::DeniedPath { path, deny } => {
                write!(f, "path `{path}` is denied by scope policy (`{deny}`)")
            }
            Self::InvalidPath { path, problem } => {
                write!(f, "scope path `{path}` is unusable: {problem}")
            }
            Self::UnknownTarget { target, known } => write!(
                f,
                "target `{target}` is not configured (known: {})",
                known.join(", ")
            ),
            Self::NeedTargetMismatch {
                unit_target,
                need_target,
            } => write!(
                f,
                "unit names target `{unit_target}` but its need names `{need_target}`"
            ),
        }
    }
}

/// Outcome of checking one declared path against a [`ScopePolicy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeDecision {
    /// The path is inside the policy.
    Allow,
    /// The path is rejected for the carried reason.
    Reject(ScopeRejection),
}

/// All-or-nothing outcome of authorizing one request against target scope
/// policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeAuthorization {
    /// Every requested path is policy-eligible; carries the request
    /// normalized, in declaration order.
    Grant(Vec<String>),
    /// The whole request is rejected; no subset is ever granted.
    Reject(ScopeRejection),
}

/// Authorize one request against a named target's policy as
/// `PumpConfig::target_configs` resolves it: an omitted `scope_allow` is
/// already default-filled to the whole repository, and `scope_deny` stays as
/// declared. An unknown target name is [`ScopeRejection::UnknownTarget`] and
/// never inherits another target's policy or the legacy default
/// configuration. Unusable policy entries fail closed. Policy eligibility is
/// not itself a need-scope grant.
#[must_use]
pub fn authorize(
    configs: &BTreeMap<String, PumpTarget>,
    target: &str,
    request: &[String],
) -> ScopeAuthorization {
    let Some(config) = configs.get(target) else {
        return ScopeAuthorization::Reject(ScopeRejection::UnknownTarget {
            target: String::from(target),
            known: configs.keys().cloned().collect(),
        });
    };
    let allow = config
        .scope_allow
        .clone()
        .unwrap_or_else(|| vec![String::from(WHOLE_REPOSITORY)]);
    match ScopePolicy::snapshot(target, &allow, &config.scope_deny) {
        Ok(policy) => policy.authorize(request),
        Err(rejection) => ScopeAuthorization::Reject(rejection),
    }
}

#[cfg(test)]
#[path = "scope_test.rs"]
mod tests;
