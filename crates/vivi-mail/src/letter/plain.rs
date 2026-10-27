//! Letter memo, rule, event and empty front matter.
//!
//! The kinds with little structure: a memo carries tags, a rule carries its
//! scope and what it supersedes, the close events carry a verdict and the
//! branch and commit they landed, and every other letter has no front matter
//! keys at all. [`render`] writes these letters; a letter whose front matter
//! is empty stores its prose as written, with no `+++` fence.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::letter::{LetterError, Validate, check_list, check_text};

/// The event verbs (`X-Vivi-Event` values) whose letters take [`CloseFront`].
pub const CLOSE_EVENTS: [&str; 2] = ["done", "reopened"];

/// Memo front matter.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoFront {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl Validate for MemoFront {
    fn validate(&self) -> Result<(), String> {
        check_list("tags", &self.tags)
    }
}

/// Where a rule applies. Absent means the whole project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleScope {
    Local,
    Project,
}

/// Rule front matter.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleFront {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<RuleScope>,
    /// Handle of the rule this one replaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
}

impl Validate for RuleFront {
    fn validate(&self) -> Result<(), String> {
        match &self.supersedes {
            Some(handle) => check_text("supersedes", handle),
            None => Ok(()),
        }
    }
}

/// Front matter of a `done` or `reopened` event letter.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseFront {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Repo name to the tip it landed at.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repos: BTreeMap<String, String>,
}

impl Validate for CloseFront {
    fn validate(&self) -> Result<(), String> {
        for (key, value) in [
            ("verdict", &self.verdict),
            ("branch", &self.branch),
            ("commit", &self.commit),
        ] {
            if let Some(text) = value {
                check_text(key, text)?;
            }
        }
        if self.branch.is_some() != self.commit.is_some() {
            return Err("`branch` and `commit` are set together or not at all".to_string());
        }
        for (repo, tip) in &self.repos {
            check_text("repos name", repo)?;
            check_text(&format!("repos.{repo}"), tip)?;
        }
        Ok(())
    }
}

/// Front matter of chat letters and of every event with no fields of its own.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyFront {}

impl Validate for EmptyFront {
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Render a memo, rule or event letter. An empty front matter stores the
/// prose as written, with no fence (ruling V1: nothing shows an empty `+++`
/// fence); any other front matter is rendered between the fences, as every
/// letter is. The front matter's own rules are checked first, so a writer
/// bug is an error at write time.
///
/// # Errors
///
/// Returns `LetterError::Invalid` when the front matter fails its own rules
/// or does not serialize.
pub fn render<T>(front: &T, prose: &str) -> Result<String, LetterError>
where
    T: Serialize + Validate,
{
    front.validate().map_err(LetterError::Invalid)?;
    let toml = toml::to_string(front)
        .map_err(|err| LetterError::Invalid(format!("front matter does not render: {err}")))?;
    if toml.trim().is_empty() {
        return Ok(prose.to_string());
    }
    crate::letter::render(front, prose)
}

#[cfg(test)]
#[path = "plain_test.rs"]
mod tests;
