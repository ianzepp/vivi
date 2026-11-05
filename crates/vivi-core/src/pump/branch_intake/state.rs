//! Versioned durable model for observations of repository branch tips.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use vivi_error::VivariumError;

pub const EXTRA_KEY: &str = "branch_intake";
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BranchIdentity {
    pub repository: String,
    pub branch: String,
}

/// SHA is deliberately not abbreviated: distinct commits may share prefixes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TipKey {
    pub branch: BranchIdentity,
    pub sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub sha: String,
    pub observed_at: u64,
    pub blocker: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedBase {
    pub generation: u64,
    pub reference: String,
    pub sha: String,
    pub identity: String,
    pub pinned_at: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateSettings {
    pub checks: Vec<String>,
    pub policy: BTreeMap<String, serde_json::Value>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub job_ids: Vec<String>,
    pub details: BTreeMap<String, serde_json::Value>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub id: String,
    pub payload: serde_json::Value,
    pub reported: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    AlreadyContained,
    PumpLanded,
    Rejected,
    Superseded,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipRecord {
    pub key: TipKey,
    pub latest: Observation,
    pub pinned_base: Option<PinnedBase>,
    pub effective_gates: GateSettings,
    pub evidence: Evidence,
    pub lifecycle: String,
    pub outcome: Option<Outcome>,
    pub successor: Option<TipKey>,
    pub created_at: u64,
    pub updated_at: u64,
    pub receipts: Vec<Receipt>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchIntakeState {
    pub version: u32,
    pub records: BTreeMap<String, TipRecord>,
    /// Compact keys remain after detailed records are retired.
    pub handled: BTreeSet<TipKey>,
}
impl Default for BranchIntakeState {
    fn default() -> Self {
        Self {
            version: VERSION,
            records: BTreeMap::new(),
            handled: BTreeSet::new(),
        }
    }
}
impl BranchIntakeState {
    pub fn from_extra(extra: &BTreeMap<String, serde_json::Value>) -> Result<Self, VivariumError> {
        let Some(value) = extra.get(EXTRA_KEY) else {
            return Ok(Self::default());
        };
        let state: Self = serde_json::from_value(value.clone())
            .map_err(|e| VivariumError::Parse(format!("branch intake state is corrupt: {e}")))?;
        if state.version != VERSION {
            return Err(VivariumError::Parse(format!(
                "unsupported branch intake state version {}",
                state.version
            )));
        }
        Ok(state)
    }
    pub fn write_extra(
        &self,
        extra: &mut BTreeMap<String, serde_json::Value>,
    ) -> Result<(), VivariumError> {
        if self.version != VERSION {
            return Err(VivariumError::Message(format!(
                "unsupported branch intake state version {}",
                self.version
            )));
        }
        let value = serde_json::to_value(self)
            .map_err(|e| VivariumError::Message(format!("serialize branch intake state: {e}")))?;
        extra.insert(EXTRA_KEY.to_owned(), value);
        Ok(())
    }
    pub fn remember(&mut self, record: TipRecord) {
        self.handled.insert(record.key.clone());
        let map_key = format!(
            "{}:{}:{}",
            record.key.branch.repository, record.key.branch.branch, record.key.sha
        );
        self.records.insert(map_key, record);
    }
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
