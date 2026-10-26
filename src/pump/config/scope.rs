//! Literal scope policy for pump targets: the `scope_allow` and `scope_deny`
//! keys of a `[pump.targets.<id>]` table.
//!
//! The policy is literal and mechanical. A path names exactly one file, one
//! directory (trailing slash, covering the whole subtree), or the repository
//! root (`.`). There are no globs: [`crate::pump::plan::overlaps`] stays the
//! glob machinery for plan checks, and this module decides at component
//! boundaries instead.
//!
//! A request is granted only when every path in it is allowed at once
//! (atomic): a request straddling the policy grants nothing. `deny` wins
//! over `allow`. A requested directory that would cover a denied descendant
//! is refused whole. An omitted `scope_allow` is repository-wide; an
//! explicit empty list authorizes nothing.
//!
//! The whole-repository default already decides scope anywhere in the
//! repository — its source, tests, docs and skills included — without an
//! operator. That is the operator's standing ruling ("scope inside the
//! repository's source, tests, docs and skills is not an operator
//! decision"), and it is honored by the broader default rather than by
//! hard-coding a directory list.
//!
//! One decision needs the filesystem: a bare request path (no trailing
//! slash, so literally a file) that is also a component-prefix of a
//! configured directory entry could name that directory on disk, and then
//! the entry would apply to it. Until filesystem verification lands in a
//! later slice, such a path fails safe as
//! [`ScopeDecision::UnverifiablePath`] and needs an operator.

use std::fmt;

use super::PumpTarget;

/// How a normalized literal path behaves in containment checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopePathKind {
    /// Exactly one file: covers only itself.
    File,
    /// A directory: covers itself and everything beneath it.
    Directory,
    /// The repository root: covers the whole repository.
    Root,
}

/// One normalized literal scope path: a repository-relative file, a
/// directory (`src/`), or the root (`.`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopePath {
    text: String,
    kind: ScopePathKind,
}

/// Why a scope entry is not a valid literal path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScopePathError {
    /// Nothing left after trimming whitespace.
    #[error("path is blank")]
    Blank,
    /// Starts with `/`; entries are repository-relative.
    #[error("path must be repository-relative, not absolute")]
    Absolute,
    /// Contains a `..` component.
    #[error("path must not contain `..`")]
    ParentDir,
    /// Starts with a drive letter and colon (`C:`).
    #[error("path must not be drive-qualified")]
    DriveQualified,
    /// Contains a glob metacharacter; policy entries are literal.
    #[error("path must not contain wildcards")]
    Wildcard,
}

impl ScopePath {
    /// Parse and normalize one literal scope path. Leading `./`, interior
    /// `.` components and repeated separators are normalized away without
    /// losing directory semantics; a trailing slash keeps the path a
    /// directory; `.` (with or without a trailing slash) is the root.
    ///
    /// # Errors
    /// [`ScopePathError`] when the text is blank, absolute, parent-containing,
    /// drive-qualified, or a wildcard.
    pub fn parse(raw: &str) -> Result<Self, ScopePathError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ScopePathError::Blank);
        }
        if trimmed.contains(['*', '?', '[', ']']) {
            return Err(ScopePathError::Wildcard);
        }
        let bytes = trimmed.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            return Err(ScopePathError::DriveQualified);
        }
        if trimmed.starts_with('/') {
            return Err(ScopePathError::Absolute);
        }
        let directory = trimmed.ends_with('/') || trimmed.ends_with("/.") || trimmed == ".";
        let mut parts = Vec::new();
        for part in trimmed.split('/') {
            match part {
                "" | "." => {}
                ".." => return Err(ScopePathError::ParentDir),
                other => parts.push(other),
            }
        }
        if parts.is_empty() {
            return Ok(Self {
                text: String::from("."),
                kind: ScopePathKind::Root,
            });
        }
        Ok(Self {
            text: if directory {
                format!("{}/", parts.join("/"))
            } else {
                parts.join("/")
            },
            kind: if directory {
                ScopePathKind::Directory
            } else {
                ScopePathKind::File
            },
        })
    }

    /// The normalized text: `src/lib.rs`, `src/`, or `.`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The path's kind: file, directory, or the root.
    #[must_use]
    pub fn kind(&self) -> ScopePathKind {
        self.kind
    }

    /// The first component (`src` of `src/lib.rs` and of `src/`); `None` for
    /// the root. Scope cuts at this component boundary, never inside one.
    #[must_use]
    pub fn first_segment(&self) -> Option<&str> {
        match self.kind {
            ScopePathKind::Root => None,
            _ => self.text.split('/').next(),
        }
    }

    /// Whether `self` covers `other`: a file covers only itself, a directory
    /// covers itself and everything beneath it at a component boundary, and
    /// the root covers the whole repository.
    #[must_use]
    pub fn covers(&self, other: &Self) -> bool {
        match self.kind {
            ScopePathKind::Root => true,
            ScopePathKind::File => other.kind == ScopePathKind::File && other.text == self.text,
            ScopePathKind::Directory => {
                other.kind != ScopePathKind::Root
                    && (other.text == self.text || other.text.starts_with(&self.text))
            }
        }
    }

    /// Whether the two paths could name overlapping scope: either covers the
    /// other. Component boundaries, not globs.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.covers(other) || other.covers(self)
    }
}

/// What the policy says about a requested write scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeDecision {
    /// Every requested path is inside the allowed scope and touches nothing
    /// denied: the request may proceed without an operator.
    Allowed,
    /// The allow list is configured and some requested path sits outside it;
    /// the whole request grants nothing.
    OutsideAllow,
    /// Some requested path touches a denied path, or a requested directory
    /// would cover a denied descendant.
    Denied,
    /// Some requested path is not a valid literal path (blank, absolute,
    /// `..`, drive-qualified, or a wildcard): an operator must rewrite it.
    InvalidPath,
    /// The decision would need the filesystem to say what a path names;
    /// until filesystem verification lands, such a request fails safe.
    UnverifiablePath,
}

impl ScopeDecision {
    /// Whether the request may proceed on the policy alone.
    #[must_use]
    pub fn is_grant(&self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// Whether an operator must decide or repair the request: only invalid
    /// and unverifiable paths need one.
    #[must_use]
    pub fn requires_operator(&self) -> bool {
        matches!(self, Self::InvalidPath | Self::UnverifiablePath)
    }
}

impl fmt::Display for ScopeDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Allowed => "allowed",
            Self::OutsideAllow => "outside the allow list",
            Self::Denied => "denied",
            Self::InvalidPath => "invalid path",
            Self::UnverifiablePath => "unverifiable path",
        };
        f.write_str(text)
    }
}

/// Blocking strength for the atomic decision: an invalid path poisons the
/// request hardest, then a denial, then an allow-list miss, then an
/// unverifiable path.
fn rank(decision: ScopeDecision) -> u8 {
    match decision {
        ScopeDecision::Allowed => 0,
        ScopeDecision::UnverifiablePath => 1,
        ScopeDecision::OutsideAllow => 2,
        ScopeDecision::Denied => 3,
        ScopeDecision::InvalidPath => 4,
    }
}

/// The literal scope policy of one target: what units may claim without an
/// operator, and what they may never claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopePolicy {
    allow: Option<Vec<ScopePath>>,
    deny: Vec<ScopePath>,
}

impl ScopePolicy {
    /// Build a policy from configured `scope_allow` / `scope_deny` entries.
    ///
    /// # Errors
    /// The first malformed entry, as [`ScopePathError`].
    pub fn new(allow: Option<Vec<String>>, deny: Vec<String>) -> Result<Self, ScopePathError> {
        let allow = allow
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| ScopePath::parse(entry))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        let deny = deny
            .iter()
            .map(|entry| ScopePath::parse(entry))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { allow, deny })
    }

    /// The policy of a configured target: the target's own `scope_allow` and
    /// `scope_deny`, unrestricted (repository-wide) when it declares none.
    ///
    /// # Errors
    /// The first malformed entry of the target.
    pub fn resolve(target: &PumpTarget) -> Result<Self, ScopePathError> {
        Self::new(target.scope_allow.clone(), target.scope_deny.clone())
    }

    /// The allow list; `None` is repository-wide.
    #[must_use]
    pub fn allow(&self) -> Option<&[ScopePath]> {
        self.allow.as_deref()
    }

    /// The deny list.
    #[must_use]
    pub fn deny(&self) -> &[ScopePath] {
        &self.deny
    }

    /// Decide a whole request atomically: the request is granted only when
    /// every path in it is allowed; any other outcome refuses the whole
    /// request and reports the strongest blocking reason (invalid paths
    /// first, then denials, then allow-list misses, then unverifiable
    /// paths). An empty request is granted.
    #[must_use]
    pub fn decide(&self, request: &[String]) -> ScopeDecision {
        let mut worst = ScopeDecision::Allowed;
        for raw in request {
            let decision = self.decide_path(raw);
            if rank(decision) > rank(worst) {
                worst = decision;
            }
        }
        worst
    }

    /// Decide one requested path against the policy.
    #[must_use]
    pub fn decide_path(&self, raw: &str) -> ScopeDecision {
        match ScopePath::parse(raw) {
            Err(_) => ScopeDecision::InvalidPath,
            Ok(path) => self.decide_one(&path),
        }
    }

    /// Decide one parsed request path: deny wins, a requested directory that
    /// would cover a denied descendant is refused, and an ambiguous bare
    /// file path fails safe before the allow list is consulted.
    fn decide_one(&self, path: &ScopePath) -> ScopeDecision {
        if self.deny.iter().any(|entry| entry.covers(path)) {
            return ScopeDecision::Denied;
        }
        if path.kind() != ScopePathKind::File && self.deny.iter().any(|entry| path.covers(entry)) {
            return ScopeDecision::Denied;
        }
        if let Some(ambiguity) = self.unverifiable(path) {
            return ambiguity;
        }
        match &self.allow {
            None => ScopeDecision::Allowed,
            Some(allow) => {
                if allow.iter().any(|entry| entry.covers(path)) {
                    ScopeDecision::Allowed
                } else {
                    ScopeDecision::OutsideAllow
                }
            }
        }
    }

    /// Why a decision must wait for the filesystem, if it must: a bare file
    /// request that is also a component-prefix of (or equal to the stem of)
    /// a configured directory entry could name that directory on disk, and
    /// then the entry would decide it. Deny ambiguity fails safe first; a
    /// request the allow list does not literally cover but a configured
    /// allow directory could name fails safe too.
    fn unverifiable(&self, path: &ScopePath) -> Option<ScopeDecision> {
        if path.kind() != ScopePathKind::File {
            return None;
        }
        let stem = format!("{}/", path.as_str());
        let shadowed = |entry: &ScopePath| {
            entry.kind() == ScopePathKind::Directory && entry.as_str().starts_with(&stem)
        };
        if self.deny.iter().any(shadowed) {
            return Some(ScopeDecision::UnverifiablePath);
        }
        let allow = self.allow.as_ref()?;
        let covered = allow.iter().any(|entry| entry.covers(path));
        if !covered && allow.iter().any(shadowed) {
            return Some(ScopeDecision::UnverifiablePath);
        }
        None
    }
}

#[cfg(test)]
#[path = "scope_test.rs"]
mod tests;
