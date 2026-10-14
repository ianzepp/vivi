//! The pump's `state.json` model, load and save.
//!
//! The shapes here are exactly pump.py's `state.json` (docs/pump/contract.md,
//! section "State file"): every field is `#[serde(default)]` so a state file
//! written by an older pump still loads, and unknown keys — top level or
//! inside a unit — land in `extra` and are written back untouched.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::VivariumError;

/// Where a unit sits in the pump lifecycle (kebab-case in `state.json`).
#[derive(Default, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Planned, waiting to be dispatched.
    #[default]
    Pending,
    /// Dispatched to an agent, still running.
    Inflight,
    /// The agent finished; awaiting merge.
    Delivered,
    /// The merge thread is working on it (in memory only; load folds this
    /// back to `Delivered`, since a crash mid-merge proves nothing).
    Merging,
    /// Merged into the integration branch.
    Merged,
    /// The agent looked and found no defect to fix.
    NoDefect,
    /// Failed too many times; parked.
    Stuck,
    /// Being split into sub-units.
    Splitting,
    /// Superseded by a replacement unit.
    Replaced,
}

impl Status {
    /// The kebab-case name used in `state.json` and in `counts`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Inflight => "inflight",
            Self::Delivered => "delivered",
            Self::Merging => "merging",
            Self::Merged => "merged",
            Self::NoDefect => "no-defect",
            Self::Stuck => "stuck",
            Self::Splitting => "splitting",
            Self::Replaced => "replaced",
        }
    }
}

/// One recorded attempt on a unit (`history` entry; `model` may be null).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Attempt {
    /// Attempt number, starting at 1.
    #[serde(default)]
    pub attempt: u32,
    /// The provider model used, when known.
    #[serde(default)]
    pub model: Option<String>,
    /// Short machine reason (`proof-failed`, `agent-replied`, ...).
    #[serde(default)]
    pub why: String,
    /// Human detail: the reply, the failing proof output, ...
    #[serde(default)]
    pub detail: String,
    /// Epoch seconds.
    #[serde(default)]
    pub at: u64,
}

/// One schedulable unit of work.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)] // field names are pump.py's state.json keys
pub struct Unit {
    /// The need this unit serves (handle without the `-N` suffix).
    #[serde(default)]
    pub need: String,
    /// One-line title.
    #[serde(default)]
    pub title: String,
    /// Full brief sent to the agent.
    #[serde(default)]
    pub brief: String,
    /// Files this unit may write.
    #[serde(default)]
    pub write_scope: Vec<String>,
    /// Unit handles that must merge first.
    #[serde(default)]
    pub after: Vec<String>,
    /// Proof commands that gate the merge.
    #[serde(default)]
    pub proof: Vec<String>,
    /// `plain` or `full`.
    #[serde(default)]
    pub tools: String,
    /// Lifecycle position.
    #[serde(default)]
    pub status: Status,
    /// Attempts so far.
    #[serde(default)]
    pub attempts: u32,
    /// Attempt log.
    #[serde(default)]
    pub history: Vec<Attempt>,
    /// Vivi task handle while inflight.
    #[serde(default)]
    pub task: Option<String>,
    /// Work branch, when created.
    #[serde(default)]
    pub branch: Option<String>,
    /// Model the current/last attempt ran on.
    #[serde(default)]
    pub model: Option<String>,
    /// Epoch seconds of unit creation.
    #[serde(default)]
    pub since: u64,
    /// Epoch seconds of the last dispatch.
    #[serde(default)]
    pub since_dispatch: Option<u64>,
    /// Pool the task was dispatched to.
    #[serde(default)]
    pub pool: Option<String>,
    /// Commit produced by the last merge.
    #[serde(default)]
    pub commit: Option<String>,
    /// Files the agent touched outside its write scope.
    #[serde(default)]
    pub outside: Vec<String>,
    /// Handles of units that replaced this one.
    #[serde(default)]
    pub replaced_by: Vec<String>,
    /// The agent's verdict text, when it replied with one.
    #[serde(default)]
    pub verdict_text: Option<String>,
    /// Unknown keys from other pump versions, written back untouched.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// What the planner decided about one need (`needs[handle]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NeedRecord {
    /// `planned`, `units`, `rejected`, ...
    #[serde(default)]
    pub verdict: String,
    /// Human explanation.
    #[serde(default)]
    pub reason: String,
    /// Unit handles minted for the need.
    #[serde(default)]
    pub units: Vec<String>,
}

/// The gate currently or last running (`last_gate`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GateRun {
    /// Epoch seconds the gate started.
    #[serde(default)]
    pub at: u64,
    /// Merge counter when it started.
    #[serde(default)]
    pub merges: u64,
    /// Vivi task handle, when the gate runs as a task.
    #[serde(default)]
    pub task: Option<String>,
    /// Integration branch tip at start.
    #[serde(default)]
    pub tip: Option<String>,
    /// Pool the gate task went to.
    #[serde(default)]
    pub pool: Option<String>,
}

/// A finished gate (`gates` entry).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GateRecord {
    /// Integration branch tip tested.
    #[serde(default)]
    pub tip: String,
    /// Epoch seconds the gate finished.
    #[serde(default)]
    pub at: u64,
    /// Merge counter at finish.
    #[serde(default)]
    pub merges: u64,
    /// Failing test names, or null when unreadable.
    #[serde(default)]
    pub names: Option<Vec<String>>,
    /// Failing names new since the last gate (absent on the baseline gate).
    #[serde(default)]
    pub new: Option<Vec<String>>,
}

/// The planner board state (`board`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Board {
    /// Epoch seconds of the last board cycle.
    #[serde(default)]
    pub at: u64,
    /// Handle -> epoch seconds the plan task started.
    #[serde(default)]
    pub planning: BTreeMap<String, u64>,
    /// `"<need>:<verdict>"` replies already recorded.
    #[serde(default)]
    pub replied: Vec<String>,
    /// Needs asked to replan.
    #[serde(default)]
    pub replan: Vec<String>,
}

/// One promoted integration tip (`promotions` entry).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Promotion {
    /// The tip promoted.
    #[serde(default)]
    pub tip: String,
    /// Epoch seconds of the promotion.
    #[serde(default)]
    pub at: u64,
}

/// The whole pump state: `state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)] // field names are pump.py's state.json keys
pub struct State {
    /// Every known unit, by handle.
    #[serde(default)]
    pub units: BTreeMap<String, Unit>,
    /// Handles in creation order.
    #[serde(default)]
    pub order: Vec<String>,
    /// Planner verdicts, by need handle.
    #[serde(default)]
    pub needs: BTreeMap<String, NeedRecord>,
    /// How many unit merges have landed.
    #[serde(default)]
    pub merges: u64,
    /// The gate currently or last running.
    #[serde(default)]
    pub last_gate: GateRun,
    /// Failing test names of the baseline gate, until then null.
    #[serde(default)]
    pub baseline: Option<Vec<String>>,
    /// Need handles closed.
    #[serde(default)]
    pub closed_needs: Vec<String>,
    /// Finished gates, oldest first.
    #[serde(default)]
    pub gates: Vec<GateRecord>,
    /// Plan file names already ingested (`"<need>.json"`).
    #[serde(default)]
    pub plans_seen: Vec<String>,
    /// Planner board state.
    #[serde(default)]
    pub board: Board,
    /// Failing test name -> unit handling it.
    #[serde(default)]
    pub red: BTreeMap<String, String>,
    /// Red names already reported to the board.
    #[serde(default)]
    pub red_reported: Vec<String>,
    /// Promoted tips, oldest first.
    #[serde(default)]
    pub promotions: Vec<Promotion>,
    /// Unknown keys from other pump versions, written back untouched.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// One unit handed to the merge thread.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct MergeJob {
    /// Unit handle.
    #[serde(default)]
    pub uid: String,
    /// Work branch to merge.
    #[serde(default)]
    pub branch: String,
    /// One-line title (commit message subject).
    #[serde(default)]
    pub title: String,
    /// Owning need handle.
    #[serde(default)]
    pub need: String,
    /// Proof commands to run on the integration branch.
    #[serde(default)]
    pub proof: Vec<String>,
    /// `plain` or `full`.
    #[serde(default)]
    pub tools: String,
    /// Files this unit may write.
    #[serde(default)]
    pub write_scope: Vec<String>,
}

/// What the merge thread reports back for one job.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct MergeResult {
    /// Unit handle.
    #[serde(default)]
    pub uid: String,
    /// Merged, failed, or unknown (null).
    #[serde(default)]
    pub ok: Option<bool>,
    /// Integration tip after the merge, when it landed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip: Option<String>,
    /// Files touched outside the write scope.
    #[serde(default)]
    pub outside: Vec<String>,
    /// Wall seconds the merge took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secs: Option<u64>,
    /// Short failure reason, when it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// Failure detail (proof output, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl State {
    /// Load the pump state from `path`.
    ///
    /// A missing file yields the empty state. Units left `merging` by a
    /// crashed run fold back to `delivered`, and an empty `plans_seen` with
    /// known needs is rebuilt as `"<need>.json"` per need — both as pump.py
    /// does at startup.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the file exists but cannot be read or
    /// parsed as JSON.
    pub fn load(path: &Path) -> Result<State, VivariumError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(err) => return Err(err.into()),
        };
        let mut state: State = serde_json::from_str(&text).map_err(|err| {
            VivariumError::Message(format!("pump state {}: {err}", path.display()))
        })?;
        state.normalize();
        Ok(state)
    }

    /// Write the state to `path` as pretty JSON with a one-space indent,
    /// like python's `json.dump(..., indent=1)`.
    ///
    /// The parent directory is created when missing; the write goes to
    /// `<path>.tmp` which is then renamed over `path`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when creating, writing, or renaming fails.
    pub fn save(&self, path: &Path) -> Result<(), VivariumError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let mut tmp_name = path.as_os_str().to_os_string();
        tmp_name.push(".tmp");
        let tmp = PathBuf::from(tmp_name);
        let file = fs::File::create(&tmp)?;
        let mut writer = std::io::BufWriter::new(file);
        let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
        let mut serializer = serde_json::Serializer::with_formatter(&mut writer, formatter);
        self.serialize(&mut serializer).map_err(|err| {
            VivariumError::Message(format!("pump state {}: {err}", path.display()))
        })?;
        writer.flush()?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Number of units per status name (kebab-case), statuses absent from
    /// the state simply missing from the map.
    #[must_use]
    pub fn counts(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for unit in self.units.values() {
            *counts.entry(unit.status.as_str().to_owned()).or_default() += 1;
        }
        counts
    }

    /// Startup repairs: fold `merging` back to `delivered`, rebuild an empty
    /// `plans_seen` from `needs`.
    fn normalize(&mut self) {
        for unit in self.units.values_mut() {
            if unit.status == Status::Merging {
                unit.status = Status::Delivered;
            }
        }
        if self.plans_seen.is_empty() && !self.needs.is_empty() {
            self.plans_seen = self
                .needs
                .keys()
                .map(|need| format!("{need}.json"))
                .collect();
        }
    }
}
