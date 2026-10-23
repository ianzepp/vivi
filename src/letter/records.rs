//! Letter pump record, pump action and mail note front matter.

use serde::{Deserialize, Serialize};

use crate::letter::sugar::opens_with_fence;
use crate::letter::work::JobFront;
use crate::letter::{LetterError, Validate, check_text, is_false, parse_letter};
use crate::pump::flow::def::FlowKind;

/// The flow a pump record asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordKind {
    Build,
    Research,
    Release,
    Repair,
}

impl RecordKind {
    /// The flow this record kind selects.
    #[must_use]
    pub fn flow_kind(self) -> FlowKind {
        match self {
            Self::Build => FlowKind::Build,
            Self::Research => FlowKind::Research,
            Self::Release => FlowKind::Release,
            Self::Repair => FlowKind::Repair,
        }
    }
}

/// Front matter of a pump record: the need a run serves and its flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordFront {
    pub need: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<RecordKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tested: Option<String>,
}

impl Validate for RecordFront {
    fn validate(&self) -> Result<(), String> {
        check_text("need", &self.need)?;
        if let Some(target) = &self.target {
            check_text("target", target)?;
        }
        if let Some(tested) = &self.tested {
            check_text("tested", tested)?;
            if self.kind != Some(RecordKind::Repair) {
                return Err("`tested` is only read on a record with kind = \"repair\": add kind = \"repair\""
                        .to_string());
            }
        }
        Ok(())
    }
}

impl RecordKind {
    fn parse(word: &str) -> Option<Self> {
        match word {
            "build" => Some(Self::Build),
            "research" => Some(Self::Research),
            "release" => Some(Self::Release),
            "repair" => Some(Self::Repair),
            _ => None,
        }
    }
}

/// The record front and prose a `pump send` body stands for. A body that opens
/// with front matter is parsed and validated as a [`RecordFront`], its `need`
/// as written. A prose body gets `need` as its front and the prose as the body.
///
/// TRANSITIONAL (W3-B3): the first `kind:`, `target:` and `tested:` prose lines
/// are lifted into the front and dropped from the prose. W3B3-25 replaces the
/// lift with a refusal.
///
/// # Errors
/// Returns the `LetterError` when front matter does not parse or validate, when
/// a `kind:` line names no record kind, or when `tested` is set without kind
/// repair.
pub fn record_letter(need: &str, body: &str) -> Result<(RecordFront, String), LetterError> {
    if opens_with_fence(body) {
        let envelope = parse_letter::<RecordFront>(body)?;
        return Ok((envelope.front, envelope.body));
    }
    let mut front = RecordFront {
        need: need.to_string(),
        kind: None,
        target: None,
        tested: None,
    };
    let (mut kind_seen, mut target_seen, mut tested_seen) = (false, false, false);
    let mut prose = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if !kind_seen && let Some(word) = trimmed.strip_prefix("kind:") {
            kind_seen = true;
            let word = word.trim();
            if !word.is_empty() {
                front.kind = Some(RecordKind::parse(word).ok_or_else(|| {
                    LetterError::Invalid(format!(
                        "pump record: expected a `kind:` line naming one of build, research, release, repair; actual: {word}"
                    ))
                })?);
            }
        } else if !target_seen && let Some(word) = trimmed.strip_prefix("target:") {
            target_seen = true;
            front.target = Some(word.trim().to_string()).filter(|word| !word.is_empty());
        } else if !tested_seen && let Some(word) = trimmed.strip_prefix("tested:") {
            tested_seen = true;
            front.tested = Some(word.trim().to_string()).filter(|word| !word.is_empty());
        } else {
            prose.push(line);
        }
    }
    front.validate().map_err(LetterError::Invalid)?;
    Ok((front, prose.join("\n").trim().to_string()))
}

/// What an operator action reply asks the pump to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionWord {
    Requeue,
    Withdraw,
    Replan,
    Answer,
    Unlock,
}

impl ActionWord {
    /// The word as written in the front matter.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Requeue => "requeue",
            Self::Withdraw => "withdraw",
            Self::Replan => "replan",
            Self::Answer => "answer",
            Self::Unlock => "unlock",
        }
    }
}

/// Front matter of a pump action: a `mail` reply on a record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionFront {
    pub action: ActionWord,
    pub need: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub fresh: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub refund: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word: Option<String>,
}

impl ActionFront {
    /// The keys that are set, each with the actions that read it.
    fn keys(&self) -> [(&'static str, bool, &'static [ActionWord]); 7] {
        use ActionWord::{Answer, Replan, Requeue, Unlock};
        [
            ("unit", self.unit.is_some(), &[Requeue]),
            ("attempts", self.attempts.is_some(), &[Requeue]),
            ("fresh", self.fresh, &[Requeue]),
            ("refund", self.refund, &[Requeue]),
            ("force", self.force, &[Replan]),
            ("step", self.step.is_some(), &[Answer]),
            ("word", self.word.is_some(), &[Answer, Unlock]),
        ]
    }

    fn require(&self, key: &str, set: bool) -> Result<(), String> {
        if set {
            return Ok(());
        }
        Err(format!(
            "`{key}` is required when action = \"{}\"",
            self.action.name()
        ))
    }
}

impl Validate for ActionFront {
    fn validate(&self) -> Result<(), String> {
        check_text("need", &self.need)?;
        for (key, value) in [
            ("unit", &self.unit),
            ("step", &self.step),
            ("word", &self.word),
        ] {
            if let Some(text) = value {
                check_text(key, text)?;
            }
        }
        for (key, set, actions) in self.keys() {
            if set && !actions.contains(&self.action) {
                return Err(format!(
                    "`{key}` is not read by action = \"{}\"",
                    self.action.name()
                ));
            }
        }
        match self.action {
            ActionWord::Requeue => self.require("unit", self.unit.is_some()),
            ActionWord::Answer => {
                self.require("step", self.step.is_some())?;
                self.require("word", self.word.is_some())
            }
            ActionWord::Withdraw | ActionWord::Replan | ActionWord::Unlock => Ok(()),
        }
    }
}

/// The marker word of a pump note, one per marked line the pump writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoteWord {
    Attempt,
    Brief,
    Deviations,
    FollowUp,
    SliceEnv,
    RepairAttempt,
    RepairRung,
    MainRed,
    Abandoned,
    SliceMerge,
    Result,
}

/// The `[pump]` table of a `mail` letter: one pump note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PumpNote {
    pub note: NoteWord,
    pub t: i64,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

/// Front matter of a `mail` letter; empty for plain mail.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailFront {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pump: Option<PumpNote>,
    /// The pump job a brief note carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<JobFront>,
}

impl Validate for MailFront {
    fn validate(&self) -> Result<(), String> {
        let brief = self
            .pump
            .as_ref()
            .is_some_and(|pump| pump.note == NoteWord::Brief);
        match (&self.job, brief) {
            (Some(_), false) => {
                return Err(
                    "`job` is only read on a brief note (pump.note = \"brief\")".to_string()
                );
            }
            (None, true) => return Err("a brief note carries a `[job]` table".to_string()),
            _ => {}
        }
        if let Some(job) = &self.job {
            job.validate()?;
        }
        let Some(pump) = &self.pump else {
            return Ok(());
        };
        if pump.t <= 0 {
            return Err("`pump.t` must be greater than zero".to_string());
        }
        if let Some(reference) = &pump.reference {
            check_text("pump.ref", reference)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "records_test.rs"]
mod tests;
