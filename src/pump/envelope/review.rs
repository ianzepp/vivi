//! Typed envelopes for the review-side roles: researcher, admitter, auditor,
//! judge, head-cto, merge and head-ceo. Judgment and reports only: no struct
//! carries a SHA, a ref, an exit code, a commit or a merged/published flag
//! (incident rule 1; facts come from runner receipts and git).

use serde::{Deserialize, Serialize};

use super::Validate;
use super::roles::Check;

fn missing(text: &Option<String>) -> bool {
    text.as_deref().is_none_or(|t| t.trim().is_empty())
}

/// The researcher answer words (without the pump's `failed`).
pub const RESEARCHER_ANSWERS: &[&str] = &["READY", "NOT READY", "NEEDS FURTHER REVIEW"];
/// The admitter answer words.
pub const ADMITTER_ANSWERS: &[&str] = &["admitted", "revise", "escalate"];
/// The auditor answer words.
pub const AUDITOR_ANSWERS: &[&str] = &["clean_pass", "residual", "block_range", "block_ship"];
/// The judge answer words.
pub const JUDGE_ANSWERS: &[&str] = &["block", "residual"];
/// The head-cto answer words.
pub const CTO_ANSWERS: &[&str] = &["research", "plan", "slice", "repair", "lock"];
/// The merge answer words.
pub const MERGE_ANSWERS: &[&str] = &["merged", "conflict"];
/// The head-ceo answer words.
pub const HEAD_CEO_ANSWERS: &[&str] = &["reviewed", "no-op"];

// ---- researcher ----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResearcherAnswer {
    #[serde(rename = "READY")]
    Ready,
    #[serde(rename = "NOT READY")]
    NotReady,
    #[serde(rename = "NEEDS FURTHER REVIEW")]
    NeedsFurtherReview,
}

/// A researcher's envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Researcher {
    pub answer: ResearcherAnswer,
    #[serde(default)]
    pub open_questions: Vec<String>,
}

impl Validate for Researcher {
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

// ---- admitter ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdmitterAnswer {
    #[serde(rename = "admitted")]
    Admitted,
    #[serde(rename = "revise")]
    Revise,
    #[serde(rename = "escalate")]
    Escalate,
}

/// An admitter's envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Admitter {
    pub answer: AdmitterAnswer,
    #[serde(default)]
    pub required_changes: Vec<String>,
    #[serde(default)]
    pub findings: Vec<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

impl Validate for Admitter {
    fn validate(&self) -> Result<(), String> {
        match self.answer {
            AdmitterAnswer::Revise if self.required_changes.is_empty() => {
                Err("answer \"revise\" requires a non-empty `required_changes`".to_string())
            }
            AdmitterAnswer::Escalate if missing(&self.reason) => {
                Err("answer \"escalate\" requires a `reason`".to_string())
            }
            _ => Ok(()),
        }
    }
}

// ---- auditor -------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditorAnswer {
    #[serde(rename = "clean_pass")]
    CleanPass,
    #[serde(rename = "residual")]
    Residual,
    #[serde(rename = "block_range")]
    BlockRange,
    #[serde(rename = "block_ship")]
    BlockShip,
}

/// The rung an auditor asks to raise the verification to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RaiseRung {
    Need,
    Main,
    Release,
}

/// Status of one requirement; mirrors `NeedRequirementStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RequirementStatus {
    Satisfied,
    Partial,
    Missing,
    Unverified,
    NotApplicable,
}

/// One `[[requirement]]` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub id: String,
    pub text: String,
    pub status: RequirementStatus,
    #[serde(default)]
    pub evidence: Vec<String>,
}

/// One `[[finding]]` row; the fields of `NeedAuditFinding`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub id: String,
    pub content: String,
    #[serde(default)]
    pub paths: Vec<String>,
    pub recheck_trigger: String,
}

/// An auditor's envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auditor {
    pub answer: AuditorAnswer,
    #[serde(default)]
    pub raise_rung: Option<RaiseRung>,
    #[serde(default)]
    pub requirement: Vec<Requirement>,
    #[serde(default)]
    pub finding: Vec<Finding>,
}

impl Validate for Auditor {
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

// ---- judge ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JudgeAnswer {
    #[serde(rename = "block")]
    Block,
    #[serde(rename = "residual")]
    Residual,
}

/// A judge's envelope (the auditor in short mode).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Judge {
    pub answer: JudgeAnswer,
    #[serde(default)]
    pub reason: Option<String>,
}

impl Validate for Judge {
    fn validate(&self) -> Result<(), String> {
        if missing(&self.reason) {
            return Err("every judge answer requires a `reason`".to_string());
        }
        Ok(())
    }
}

// ---- head-cto ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CtoAnswer {
    #[serde(rename = "research")]
    Research,
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "slice")]
    Slice,
    #[serde(rename = "repair")]
    Repair,
    #[serde(rename = "lock")]
    Lock,
}

/// A head-cto envelope. `new_fact` is optional here; the run-3 rule belongs
/// to the charter, not the parser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cto {
    pub answer: CtoAnswer,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub new_fact: Option<String>,
}

impl Validate for Cto {
    fn validate(&self) -> Result<(), String> {
        if missing(&self.reason) {
            return Err("every head-cto answer requires a `reason`".to_string());
        }
        Ok(())
    }
}

// ---- merge ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeAnswer {
    #[serde(rename = "merged")]
    Merged,
    #[serde(rename = "conflict")]
    Conflict,
}

/// A merge step's envelope: the worker's claim and its judgment fields only.
/// The published SHA and the merged fact come from the runner receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Merge {
    pub answer: MergeAnswer,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub design_change: bool,
    #[serde(default)]
    pub check: Vec<Check>,
}

impl Validate for Merge {
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

// ---- head-ceo ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeadCeoAnswer {
    #[serde(rename = "reviewed")]
    Reviewed,
    #[serde(rename = "no-op")]
    NoOp,
}

/// A head-ceo envelope (the cadence prompt; not acted on in wave 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeadCeo {
    pub answer: HeadCeoAnswer,
    #[serde(default)]
    pub promoted: Vec<String>,
    #[serde(default)]
    pub research: Vec<String>,
    #[serde(default)]
    pub dropped: Vec<String>,
}

impl Validate for HeadCeo {
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "review_test.rs"]
mod tests;
