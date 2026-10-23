//! Letter need, want and task front matter.

use serde::{Deserialize, Serialize};

use crate::letter::{Validate, check_list, check_text, is_false};

/// Front matter of a need: the unit of delegated work.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeedFront {
    /// What must be true for the need to be done. Required and non-empty.
    pub done_when: Vec<String>,
    /// The repo or checkout the need targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Handles of the needs this one waits for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Paths the need may change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_scope: Vec<String>,
    /// Pump ordering priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    /// The want this need serves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serves: Option<String>,
}

impl Validate for NeedFront {
    fn validate(&self) -> Result<(), String> {
        if self.done_when.is_empty() {
            return Err(
                "need has no done_when: add done_when = [...] to the front matter".to_string(),
            );
        }
        check_list("done_when", &self.done_when)?;
        check_opt("target", self.target.as_deref())?;
        check_list("depends_on", &self.depends_on)?;
        check_list("write_scope", &self.write_scope)?;
        check_opt("serves", self.serves.as_deref())
    }
}

/// Front matter of a want: a question or goal to explore.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WantFront {
    /// The question or goal the want explores. Required.
    pub question: String,
    /// Handles of the wants this one waits for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

impl Validate for WantFront {
    fn validate(&self) -> Result<(), String> {
        check_text("question", &self.question)?;
        check_list("depends_on", &self.depends_on)
    }
}

/// Front matter of a task: one step of a need.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFront {
    /// What must be true for the task to be done.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub done_when: Vec<String>,
    /// Handles of the tasks this one waits for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Handle of the parent task or need.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The repo or checkout the task targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Paths the task may change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_scope: Vec<String>,
    /// The pool job base ref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The pool job branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Extra pool job inputs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub with: Vec<String>,
    /// The pool band, parsed by the pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band: Option<String>,
    /// The pump step this task belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<StepFront>,
}

impl Validate for TaskFront {
    fn validate(&self) -> Result<(), String> {
        check_list("done_when", &self.done_when)?;
        check_list("depends_on", &self.depends_on)?;
        check_opt("parent", self.parent.as_deref())?;
        check_opt("target", self.target.as_deref())?;
        check_list("write_scope", &self.write_scope)?;
        check_opt("from", self.from.as_deref())?;
        check_opt("branch", self.branch.as_deref())?;
        check_list("with", &self.with)?;
        check_opt("band", self.band.as_deref())?;
        self.step.as_ref().map_or(Ok(()), Validate::validate)
    }
}

/// The `[step]` table of a task: which pump run and node it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepFront {
    /// Handle of the need the step belongs to. Required.
    pub need: String,
    /// The run number. Required.
    pub run: u32,
    /// The flow node. Required unless this is the run sentinel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// True for the run sentinel, which has no node of its own.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sentinel: bool,
}

impl Validate for StepFront {
    fn validate(&self) -> Result<(), String> {
        check_text("step.need", &self.need)?;
        match (&self.node, self.sentinel) {
            (Some(_), true) => {
                Err("`step.node` must be absent when `step.sentinel` is true".into())
            }
            (Some(node), false) => check_text("step.node", node),
            (None, false) => Err("`step.node` is required unless `step.sentinel` is true".into()),
            (None, true) => Ok(()),
        }
    }
}

/// An optional text field must not be blank when set.
fn check_opt(key: &str, value: Option<&str>) -> Result<(), String> {
    value.map_or(Ok(()), |text| check_text(key, text))
}

#[cfg(test)]
#[path = "work_test.rs"]
mod tests;
