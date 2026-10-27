//! Literal scope paths and typed scope decisions for pump authorization.
//!
//! A scope allow-list is a set of literal repository-relative entries, not
//! glob patterns. An entry is either an exact file (`src/a.rs`) or a
//! directory subtree (`src/`, trailing slash); `./` denotes the repository
//! root. [`ScopePath`] normalizes one entry and answers containment and
//! overlap questions; [`ScopeDecision`] is the serialized decision code a
//! scope check reports.
//!
//! The `flow::scope` authorization route and the allocation code that
//! trims work to granted paths both build on these primitives, so path
//! matching lives here and nowhere else.

use crate::error::VivariumError;

/// The repository root, spelled `./`.
const ROOT: &str = "./";

/// One literal, normalized, repository-relative scope path.
///
/// Either an exact file (`src/a.rs`) or a directory subtree (`src/`,
/// trailing slash). `./` is the repository root and grants everything.
/// Construct only through [`ScopePath::normalize`]; the stored spelling is
/// already free of `./` segments and repeated separators.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopePath {
    text: String,
}

impl ScopePath {
    /// Normalize `raw` into a scope path.
    ///
    /// Harmless `.` segments and repeated separators are collapsed and the
    /// whole entry is trimmed. Blank entries, absolute paths and anything
    /// with a `..` component are refused: a scope entry may never escape
    /// the repository.
    pub fn normalize(raw: &str) -> Result<Self, VivariumError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(VivariumError::Config("scope path is blank".to_owned()));
        }
        if trimmed.starts_with('/') {
            return Err(VivariumError::Config(format!(
                "scope path `{trimmed}` is absolute; scope paths are repository-relative"
            )));
        }
        let directory = trimmed == "." || trimmed.ends_with('/');
        let mut components: Vec<&str> = Vec::new();
        for component in trimmed.split('/') {
            match component {
                "" | "." => {}
                ".." => {
                    return Err(VivariumError::Config(format!(
                        "scope path `{raw}` traverses above the repository"
                    )));
                }
                _ => components.push(component),
            }
        }
        let text = if components.is_empty() {
            ROOT.to_owned()
        } else {
            let mut text = components.join("/");
            if directory {
                text.push('/');
            }
            text
        };
        Ok(Self { text })
    }

    /// The normalized spelling, e.g. `src/` or `src/a.rs`.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// True when the entry is the repository root (`./`).
    pub fn is_root(&self) -> bool {
        self.text == ROOT
    }

    /// True when the entry is a directory subtree (trailing slash).
    pub fn is_directory(&self) -> bool {
        self.text.ends_with('/')
    }

    /// Component-aware containment: does this entry grant `other`?
    ///
    /// The root grants everything; an exact file grants only itself; a
    /// directory grants itself and every entry under it. `src/` contains
    /// `src/a.rs` but not `src-old/a.rs`, and `src/a.rs` does not contain
    /// `src/a.rs/child`.
    pub fn contains(&self, other: &Self) -> bool {
        if self.is_root() {
            return true;
        }
        if self.text == other.text {
            return true;
        }
        self.is_directory() && other.text.starts_with(&self.text)
    }

    /// Symmetric overlap: could both entries claim the same file?
    ///
    /// True when either side contains the other, so `src/` and `src/a.rs`
    /// overlap while `src/a.rs` and `src-old/a.rs` do not.
    pub fn overlaps(&self, other: &Self) -> bool {
        self.contains(other) || other.contains(self)
    }
}

impl std::fmt::Display for ScopePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// Typed decision code from a scope check, serialized as its snake_case
/// code: `granted`, `outside_allow`, `scope_denied`, `unsafe_path` or
/// `unknown_target`. A target mismatch carries the offending target id in
/// the variant, so callers never inspect prose to learn which target was
/// unknown.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeDecision {
    /// Every requested path falls inside the target's allowed scope.
    Granted,
    /// The target is known but at least one path falls outside its allow
    /// list.
    OutsideAllow,
    /// The request is refused outright by the published scope ruling.
    ScopeDenied,
    /// A path is blank, absolute or traverses above the repository.
    UnsafePath,
    /// The request names a target that no `[pump.targets.<id>]` configures.
    UnknownTarget {
        /// The target id that is not configured.
        target: String,
    },
}

impl ScopeDecision {
    /// The stable decision code, e.g. `granted` or `unknown_target`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::OutsideAllow => "outside_allow",
            Self::ScopeDenied => "scope_denied",
            Self::UnsafePath => "unsafe_path",
            Self::UnknownTarget { .. } => "unknown_target",
        }
    }

    /// The unconfigured target id when the code is `unknown_target`.
    pub fn unknown_target(&self) -> Option<&str> {
        match self {
            Self::UnknownTarget { target } => Some(target),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "scope_policy_test.rs"]
mod tests;
