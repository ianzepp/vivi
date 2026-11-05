//! Typed envelopes per role. Hand, slicer and planner live here; the other
//! roles are in `review.rs`.

use serde::{Deserialize, Serialize};

use super::review::{Admitter, Auditor, Cto, HeadCeo, Judge, Merge, Researcher};
use super::{Envelope, FENCE, LetterError as EnvelopeError, Validate, parse};

/// Which role wrote a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleKind {
    Researcher,
    Planner,
    Admitter,
    Slicer,
    Auditor,
    Judge,
    Cto,
    Merge,
    Hand,
    Repairer,
    HeadCeo,
}

impl RoleKind {
    /// Every role, in declaration order.
    pub const ALL: [RoleKind; 11] = [
        Self::Researcher,
        Self::Planner,
        Self::Admitter,
        Self::Slicer,
        Self::Auditor,
        Self::Judge,
        Self::Cto,
        Self::Merge,
        Self::Hand,
        Self::Repairer,
        Self::HeadCeo,
    ];

    /// The role's name, as a seat or a person says it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Researcher => "researcher",
            Self::Planner => "planner",
            Self::Admitter => "admitter",
            Self::Slicer => "slicer",
            Self::Auditor => "auditor",
            Self::Judge => "judge",
            Self::Cto => "cto",
            Self::Merge => "merge",
            Self::Hand => "hand",
            Self::Repairer => "repairer",
            Self::HeadCeo => "head-ceo",
        }
    }

    /// The role named `name` (as [`RoleKind::name`] spells it).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// The `answer` words the role's envelope accepts.
    pub fn answer_words(self) -> &'static [&'static str] {
        match self {
            Self::Researcher => &["READY", "NOT READY", "NEEDS FURTHER REVIEW"],
            Self::Planner => &["planned", "delivered", "operator", "hold", "design"],
            Self::Admitter => &["admitted", "revise", "escalate"],
            Self::Slicer => &["sliced", "escalate"],
            Self::Auditor => &["clean_pass", "residual", "block_range", "block_ship"],
            Self::Judge => &["block", "residual"],
            Self::Cto => &["research", "plan", "slice", "rework", "lock"],
            Self::Merge => &["merged", "conflict"],
            Self::Hand => &["built", "blocked-scope", "failed"],
            Self::Repairer => &["built", "failed"],
            Self::HeadCeo => &["reviewed", "no-op"],
        }
    }
}

/// A reply parsed for its role, with the front matter as JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub answer: String,
    pub front: serde_json::Value,
    pub body: String,
}

/// Parse a reply for the given role.
pub fn parse_for(kind: RoleKind, text: &str) -> Result<Parsed, EnvelopeError> {
    match kind {
        RoleKind::Hand => parsed::<Hand>(text),
        RoleKind::Repairer => parsed::<Repairer>(text),
        RoleKind::Slicer => parsed::<Slicer>(text),
        RoleKind::Planner => parsed::<Planner>(text),
        RoleKind::Researcher => parsed::<Researcher>(text),
        RoleKind::Admitter => parsed::<Admitter>(text),
        RoleKind::Auditor => parsed::<Auditor>(text),
        RoleKind::Judge => parsed::<Judge>(text),
        RoleKind::Cto => parsed::<Cto>(text),
        RoleKind::Merge => parsed::<Merge>(text),
        RoleKind::HeadCeo => parsed::<HeadCeo>(text),
    }
}

/// Parse one role's reply envelope, the way every reader of a role reply
/// must: strictly, else after a prose preamble (see [`after_preamble`]). The
/// pump's collectors, its slicer fan-out and its hand classifier all read the
/// same reply, so they share this one rule and cannot disagree on a word.
///
/// # Errors
/// Returns [`EnvelopeError`] when no fence line opens a valid envelope.
pub fn parse_role_reply<T>(text: &str) -> Result<Envelope<T>, EnvelopeError>
where
    T: serde::de::DeserializeOwned + Validate,
{
    match parse::<T>(text) {
        Err(EnvelopeError::NoOpening) => after_preamble::<T>(text),
        other => other,
    }
}

fn parsed<T>(text: &str) -> Result<Parsed, EnvelopeError>
where
    T: serde::de::DeserializeOwned + Serialize + Validate,
{
    let envelope = parse_role_reply::<T>(text)?;
    let front = serde_json::to_value(&envelope.front)
        .map_err(|err| EnvelopeError::Invalid(err.to_string()))?;
    let answer = front
        .get("answer")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| EnvelopeError::Invalid("missing answer".to_string()))?
        .to_string();
    Ok(Parsed {
        answer,
        front,
        body: envelope.body,
    })
}

/// A reply that opens with prose before its envelope ("All checks pass. Final
/// receipt:" then `+++`): parse from the first line that is only the fence and
/// keep the preamble at the head of the body, so no fact is lost. A reply with
/// no fence line at all stays [`EnvelopeError::NoOpening`]. Letters elsewhere
/// keep the strict rule; only role replies, which models often introduce, are
/// read this way.
fn after_preamble<T>(text: &str) -> Result<Envelope<T>, EnvelopeError>
where
    T: serde::de::DeserializeOwned + Validate,
{
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == FENCE {
            let preamble = text[..offset].trim();
            let mut envelope = parse::<T>(&text[offset..])?;
            if !preamble.is_empty() {
                envelope.body = if envelope.body.is_empty() {
                    preamble.to_string()
                } else {
                    format!("{preamble}\n\n{}", envelope.body)
                };
            }
            return Ok(envelope);
        }
        offset += line.len();
    }
    Err(EnvelopeError::NoOpening)
}

fn blank(text: &str) -> bool {
    text.trim().is_empty()
}

// ---- hand ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandAnswer {
    #[serde(rename = "built")]
    Built,
    #[serde(rename = "blocked-scope")]
    BlockedScope,
    #[serde(rename = "failed")]
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckResult {
    Pass,
    Fail,
    Skipped,
}

/// One `[[check]]` row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub cmd: String,
    pub result: CheckResult,
    #[serde(deserialize_with = "deserialize_seconds")]
    pub seconds: f64,
    /// Optional free-text note on the check (for example which tests failed).
    /// Models add it often; refusing the whole envelope over it lost rounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `seconds` is a duration: any finite, non-negative number, TOML integer or
/// float alike. Fractions are preserved and signed zero accepted; the number
/// must stay native so a string or a negative or non-finite value is refused
/// at deserialization, under the ordinary TOML error path.
fn deserialize_seconds<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let seconds = toml::Value::deserialize(deserializer)?;
    match seconds {
        toml::Value::Integer(n) if n >= 0 => Ok(n as f64),
        toml::Value::Float(n) if n.is_finite() && n >= 0.0 => Ok(n),
        other => Err(serde::de::Error::custom(format!(
            "`seconds` must be a finite, non-negative number, found {}",
            other.type_str()
        ))),
    }
}

/// One `[[follow_up]]` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FollowUp {
    pub paths: Vec<String>,
    pub need: String,
    /// An optional follow-up is posted on the slice task only; a required one
    /// goes to the slicer in rework mode.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

/// A hand's envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hand {
    pub answer: HandAnswer,
    #[serde(default)]
    pub change: Vec<String>,
    #[serde(default)]
    pub check: Vec<Check>,
    #[serde(default)]
    pub deviations: Vec<String>,
    #[serde(default)]
    pub follow_up: Vec<FollowUp>,
    #[serde(default)]
    pub remaining: Vec<String>,
    #[serde(default)]
    pub scope_paths: Vec<String>,
    #[serde(default)]
    pub scope_reason: Option<String>,
    #[serde(default)]
    pub scope_design: bool,
}

impl Validate for Hand {
    fn validate(&self) -> Result<(), String> {
        if self.answer != HandAnswer::BlockedScope {
            return Ok(());
        }
        if self.scope_paths.is_empty() {
            return Err("answer \"blocked-scope\" requires a non-empty `scope_paths`".to_string());
        }
        if self.scope_reason.as_deref().is_none_or(blank) {
            return Err("answer \"blocked-scope\" requires a `scope_reason`".to_string());
        }
        Ok(())
    }
}

// ---- repairer ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepairerAnswer {
    #[serde(rename = "built")]
    Built,
    #[serde(rename = "failed")]
    Failed,
}

/// A repairer's envelope. `overtime` is the collector's word for a job past
/// its time cap, never a reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repairer {
    pub answer: RepairerAnswer,
    #[serde(default)]
    pub change: Vec<String>,
    #[serde(default)]
    pub check: Vec<Check>,
    #[serde(default)]
    pub deviations: Vec<String>,
}

impl Validate for Repairer {
    /// No diff cap: a top-tier repairer may need a large fix.
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

// ---- planner -------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlannerAnswer {
    #[serde(rename = "planned")]
    Planned,
    #[serde(rename = "delivered")]
    Delivered,
    #[serde(rename = "operator")]
    Operator,
    #[serde(rename = "hold")]
    Hold,
    #[serde(rename = "design")]
    Design,
}

/// A planner's envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Planner {
    pub answer: PlannerAnswer,
    #[serde(default)]
    pub reason: Option<String>,
}

impl Validate for Planner {
    fn validate(&self) -> Result<(), String> {
        if self.answer != PlannerAnswer::Planned && self.reason.as_deref().is_none_or(blank) {
            return Err("every planner answer except \"planned\" requires a `reason`".to_string());
        }
        Ok(())
    }
}

// ---- slicer --------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlicerAnswer {
    #[serde(rename = "sliced")]
    Sliced,
    #[serde(rename = "escalate")]
    Escalate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlicerMode {
    #[default]
    Slice,
    Rework,
    Split,
}

/// One `[[slice]]` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slice {
    pub id: String,
    pub title: String,
    pub brief: String,
    pub target: String,
    #[serde(default)]
    pub write_scope: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
}

/// A slicer's envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slicer {
    pub answer: SlicerAnswer,
    #[serde(default)]
    pub mode: SlicerMode,
    #[serde(default)]
    pub slice: Vec<Slice>,
    #[serde(default)]
    pub reason: Option<String>,
}

impl Validate for Slicer {
    fn validate(&self) -> Result<(), String> {
        match self.answer {
            SlicerAnswer::Escalate => {
                if !self.slice.is_empty() {
                    return Err("answer \"escalate\" takes no `[[slice]]` rows".to_string());
                }
                if self.reason.as_deref().is_none_or(blank) {
                    return Err("answer \"escalate\" requires a `reason`".to_string());
                }
            }
            SlicerAnswer::Sliced => {
                if self.slice.is_empty() {
                    return Err(
                        "answer \"sliced\" requires at least one `[[slice]]` row".to_string()
                    );
                }
                if self.mode == SlicerMode::Split {
                    self.validate_split()?;
                }
            }
        }
        Ok(())
    }
}

impl Slicer {
    /// Split ids are `<old id>.<n>`, two to four of them.
    fn validate_split(&self) -> Result<(), String> {
        if !(2..=4).contains(&self.slice.len()) {
            return Err("a split gives 2 to 4 `[[slice]]` rows".to_string());
        }
        for slice in &self.slice {
            let numbered = slice
                .id
                .rsplit_once('.')
                .is_some_and(|(old, n)| !old.is_empty() && n.parse::<u32>().is_ok());
            if !numbered {
                return Err(format!(
                    "split slice id `{}` must be `<old id>.<n>`",
                    slice.id
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "roles_test.rs"]
mod tests;
