//! The shared branch-intake records. These are the values the store writes
//! and every later intake step reads; they carry no I/O.

use std::fmt;

use serde::{Deserialize, Serialize};
use vivi_error::VivariumError;

/// Where one retained generation stands.
///
/// `landed`, `conflict`, `red` and `superseded` are the terminal words.
/// Containment and a disabled source cancel a generation without calling it
/// red or green; infrastructure and proof holds are explicit non-verdicts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GenerationStage {
    #[default]
    Preparing,
    Constructed,
    Proving,
    Ready,
    Landed,
    Conflict,
    Red,
    Superseded,
    Contained,
    DisabledSource,
    InfrastructureHold,
    ProofHold,
}

impl GenerationStage {
    /// Every word the store accepts, in vocabulary order.
    pub const ALL: [GenerationStage; 12] = [
        Self::Preparing,
        Self::Constructed,
        Self::Proving,
        Self::Ready,
        Self::Landed,
        Self::Conflict,
        Self::Red,
        Self::Superseded,
        Self::Contained,
        Self::DisabledSource,
        Self::InfrastructureHold,
        Self::ProofHold,
    ];

    /// The stored word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Constructed => "constructed",
            Self::Proving => "proving",
            Self::Ready => "ready",
            Self::Landed => "landed",
            Self::Conflict => "conflict",
            Self::Red => "red",
            Self::Superseded => "superseded",
            Self::Contained => "contained",
            Self::DisabledSource => "disabled-source",
            Self::InfrastructureHold => "infrastructure-hold",
            Self::ProofHold => "proof-hold",
        }
    }

    /// Parse a stored word; an unknown word is an error, never an empty state.
    ///
    /// # Errors
    /// Returns [`VivariumError::Parse`] for a word outside [`Self::ALL`].
    pub fn parse(word: &str) -> Result<Self, VivariumError> {
        for stage in Self::ALL {
            if stage.as_str() == word {
                return Ok(stage);
            }
        }
        Err(VivariumError::Parse(format!(
            "branch intake: expected a generation stage in {}; actual: {word}",
            Self::ALL
                .iter()
                .map(|stage| stage.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }

    /// True for the four terminal words.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Landed | Self::Conflict | Self::Red | Self::Superseded
        )
    }

    /// True when the generation was cancelled by containment or a disabled source.
    #[must_use]
    pub fn is_cancelled(self) -> bool {
        matches!(self, Self::Contained | Self::DisabledSource)
    }

    /// True for a hold that is neither a red nor a green result.
    #[must_use]
    pub fn is_hold(self) -> bool {
        matches!(self, Self::InfrastructureHold | Self::ProofHold)
    }
}

impl fmt::Display for GenerationStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The latest source/main observation for one branch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchObservation {
    /// The full source commit SHA that was observed.
    pub source_sha: String,
    /// The main commit the source was observed against.
    pub observed_main: String,
    /// Epoch seconds.
    pub observed_at: u64,
}

/// One tip in a branch's history, keyed by its full source SHA.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipRecord {
    pub target: String,
    pub source_ref: String,
    pub source_sha: String,
    pub observed_main: String,
    pub observed_at: u64,
}

/// Whether a notification is owed and when it was delivered.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationState {
    pub needed: bool,
    pub delivered_at: Option<u64>,
}

/// Whether a publication is owed and when it happened.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationState {
    pub needed: bool,
    pub published_at: Option<u64>,
}

/// One retained generation. Earlier generations are never replaced by a
/// later one: their provenance stays readable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationRecord {
    pub target: String,
    pub source_ref: String,
    pub generation: u64,
    pub observed_main: String,
    pub candidate_sha: String,
    pub candidate_ref: String,
    pub construction_author: String,
    pub construction_committer: String,
    pub construction_message: String,
    pub constructed_at: u64,
    pub stage: GenerationStage,
    pub reasons: Vec<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl GenerationRecord {
    /// Replace the stage and stamp the update time.
    pub fn set_stage(&mut self, stage: GenerationStage, at: u64) {
        self.stage = stage;
        self.updated_at = at;
    }
}

/// A proof purpose, kept as a string so a later rung can name a new purpose
/// without a storage migration.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProofPurpose(pub String);

impl ProofPurpose {
    /// A purpose from any string-like value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The stored purpose.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ProofPurpose {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ProofPurpose {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl fmt::Display for ProofPurpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One saved proof of a generation, including the exact inputs that produced
/// it and the evidence it names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofRecord {
    pub purpose: ProofPurpose,
    pub target: String,
    pub source_ref: String,
    pub generation: u64,
    pub tip: String,
    pub rung: String,
    pub command: String,
    pub fingerprint: String,
    pub job: String,
    pub task: String,
    /// Serialized execution inputs, saved exactly as the rung saw them.
    pub inputs: String,
    /// Serialized execution results, saved exactly as the rung saw them.
    pub results: String,
    /// Evidence identities (message handles, artifact names, ...).
    pub evidence: Vec<String>,
    /// The original lines the proof selected.
    pub lines: Vec<String>,
    pub created_at: u64,
}

/// A durable notification or publication intent. Delivering the intent marks
/// it; it is never deleted by a normal State save.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntakeIntentKind {
    #[default]
    Notification,
    Publication,
}

impl IntakeIntentKind {
    /// The stored word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Notification => "notification",
            Self::Publication => "publication",
        }
    }

    /// Parse a stored word.
    ///
    /// # Errors
    /// Returns [`VivariumError::Parse`] for an unknown intent kind.
    pub fn parse(word: &str) -> Result<Self, VivariumError> {
        match word {
            "notification" => Ok(Self::Notification),
            "publication" => Ok(Self::Publication),
            other => Err(VivariumError::Parse(format!(
                "branch intake: expected intent notification or publication; actual: {other}"
            ))),
        }
    }
}

/// One durable notification/publication intent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntakeIntent {
    /// Database identity; zero before the row is written.
    pub id: i64,
    pub kind: IntakeIntentKind,
    pub target: String,
    pub source_ref: String,
    pub generation: Option<u64>,
    pub candidate_ref: Option<String>,
    pub candidate_sha: Option<String>,
    /// The saved notification text, or an empty publication payload.
    pub body: String,
    pub created_at: u64,
    pub delivered_at: Option<u64>,
}

/// Everything known about one `(target, canonical source ref)` branch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchState {
    pub target: String,
    /// The canonical source ref, for example `refs/heads/vivi/unit/x`.
    pub source_ref: String,
    pub latest_observation: Option<BranchObservation>,
    pub active_generation: Option<u64>,
    pub notification: NotificationState,
    pub publication: PublicationState,
    /// Tip history, keyed by full source SHA and kept oldest first.
    pub tips: Vec<TipRecord>,
    /// Retained generations, kept oldest first.
    pub generations: Vec<GenerationRecord>,
    /// Saved proofs, kept in insertion order.
    pub proofs: Vec<ProofRecord>,
    /// Durable intents, kept in insertion order.
    pub intents: Vec<IntakeIntent>,
    pub updated_at: u64,
}

impl BranchState {
    /// A new empty state for one branch.
    #[must_use]
    pub fn new(target: impl Into<String>, source_ref: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            source_ref: source_ref.into(),
            ..Self::default()
        }
    }

    /// Retain `tip`, replacing an earlier row with the same full SHA.
    pub fn record_tip(&mut self, tip: TipRecord) {
        if let Some(existing) = self
            .tips
            .iter_mut()
            .find(|existing| existing.source_sha == tip.source_sha)
        {
            *existing = tip;
        } else {
            self.tips.push(tip);
        }
    }

    /// Read a tip by its full source SHA.
    #[must_use]
    pub fn tip(&self, source_sha: &str) -> Option<&TipRecord> {
        self.tips.iter().find(|tip| tip.source_sha == source_sha)
    }

    /// Retain `generation`, replacing the row for the same generation number
    /// while leaving every other generation untouched.
    pub fn record_generation(&mut self, generation: GenerationRecord) {
        if let Some(existing) = self
            .generations
            .iter_mut()
            .find(|existing| existing.generation == generation.generation)
        {
            *existing = generation;
        } else {
            self.generations.push(generation);
        }
    }

    /// Read one retained generation.
    #[must_use]
    pub fn generation(&self, generation: u64) -> Option<&GenerationRecord> {
        self.generations
            .iter()
            .find(|record| record.generation == generation)
    }
}
