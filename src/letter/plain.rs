//! Letter memo, rule, event and empty front matter.
//!
//! The kinds with little structure: a memo carries tags, a rule carries its
//! scope and what it supersedes, the close events carry a verdict and the
//! branch and commit they landed, and every other letter has no front matter
//! keys at all.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::letter::{Validate, check_list, check_text};

/// The event verbs (`X-Vivi-Event` values) whose letters take [`CloseFront`].
pub const CLOSE_EVENTS: [&str; 3] = ["done", "reopened", "promoted"];

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

/// Front matter of a `done`, `reopened` or `promoted` event letter.
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

#[cfg(test)]
#[path = "plain_test.rs"]
mod tests;
