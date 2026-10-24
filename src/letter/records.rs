//! Letter pump record, pump action and mail note front matter.

use serde::{Deserialize, Serialize};

use crate::letter::plain::CloseFront;
use crate::letter::sugar::opens_with_fence;
use crate::letter::work::JobFront;
use crate::letter::{LetterError, Validate, check_text, parse_letter};
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

/// The record front and prose a `pump send` body stands for. A body that opens
/// with front matter is parsed and validated as a [`RecordFront`], its `need`
/// as written. A prose body gets `need` as its front and the prose as the body.
///
/// # Errors
/// Returns the `LetterError` when front matter does not parse or validate, or
/// when a prose line starts with `kind:`, `target:` or `tested:`: those facts
/// go in front matter.
pub fn record_letter(need: &str, body: &str) -> Result<(RecordFront, String), LetterError> {
    if opens_with_fence(body) {
        let envelope = parse_letter::<RecordFront>(body)?;
        return Ok((envelope.front, envelope.body));
    }
    let fact_line = body.lines().any(|line| {
        let line = line.trim_start();
        ["kind:", "target:", "tested:"]
            .iter()
            .any(|key| line.starts_with(key))
    });
    if fact_line {
        return Err(LetterError::Invalid(
            "pump record facts go in front matter, not prose lines: write need = \"<handle>\" (required) and kind = \"..\", target = \"..\" or tested = \"..\" between +++ fences"
                .to_string(),
        ));
    }
    let front = RecordFront {
        need: need.to_string(),
        kind: None,
        target: None,
        tested: None,
    };
    front.validate().map_err(LetterError::Invalid)?;
    Ok((front, body.trim().to_string()))
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
    pub step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word: Option<String>,
}

impl ActionFront {
    /// The keys that are set, each with the actions that read it.
    fn keys(&self) -> [(&'static str, bool, &'static [ActionWord]); 3] {
        use ActionWord::{Answer, Requeue, Unlock};
        [
            ("unit", self.unit.is_some(), &[Requeue]),
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

/// The `[pool]` table of a `mail` letter: one pool event note on a task,
/// carrying the machine-readable record the event log holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolNote {
    /// The event word (`pool_dispatched`, `pool_collected`).
    pub event: String,
    /// The JSON record of the event, as the event log stores it.
    pub record: String,
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
    /// The close facts a report note carries: a close by a role that may
    /// not write the event letter reports its verdict, repo tips and
    /// integration branch/commit here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close: Option<CloseFront>,
    /// The pool event a pool note carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<PoolNote>,
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
        if let Some(close) = &self.close {
            close.validate()?;
        }
        if let Some(pool) = &self.pool {
            check_text("pool.event", &pool.event)?;
            check_text("pool.record", &pool.record)?;
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
