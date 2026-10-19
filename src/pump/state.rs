//! The pump's `state.json` model, load and save.
//!
//! The shapes here are the `state.json` shape (skills/vivi/references/pump-contract.md,
//! section "State file"): every field is `#[serde(default)]` so a state file
//! written by an older pump still loads, and unknown keys — top level or
//! inside a unit — land in `extra` and are written back untouched.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::VivariumError;

/// Where a unit sits in the pump lifecycle (kebab-case in `state.json`).
///
/// The set is open: a status the pump does not know (set by hand, or by a
/// newer pump) loads as [`Status::Other`] and is written back exactly as it
/// was read. Such a unit is never dispatched and never satisfies an `after`
/// dependency, exactly like [`Status::Withdrawn`].
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// Planned, waiting to be dispatched.
    #[default]
    Pending,
    /// Temporarily blocked because its configured repository target is absent.
    Held,
    /// Dispatched to an agent, still running.
    Inflight,
    /// The agent finished; awaiting merge.
    Delivered,
    /// A merge lane has taken it and is working on it: the unit is inside
    /// exactly one lane's batch (in memory only; load folds this back to
    /// `Delivered`, since a crash mid-merge proves nothing). Units waiting
    /// for a free lane stay `Delivered`.
    Merging,
    /// Merged into the integration branch.
    Merged,
    /// Merged into a batch whose shared check failed. The batch is set aside
    /// on a `pump/quarantine/<id>` branch and one repair unit works on it
    /// (`Unit::quarantine` names the id). Not fed to a lane, not dispatched,
    /// not satisfied; it becomes `Merged` when the repair unit merges, or
    /// returns to `Delivered` (solo) when the repair unit goes `Stuck`.
    Quarantined,
    /// The agent looked and found no defect to fix.
    NoDefect,
    /// Failed too many times; parked.
    Stuck,
    /// Parked because its plan is defective (proof usage error); waits for a
    /// revision, never dispatched.
    NeedsReplan,
    /// Being split into sub-units.
    Splitting,
    /// Superseded by a replacement unit.
    Replaced,
    /// Taken out of the queue by hand; the pump leaves it alone.
    Withdrawn,
    /// Any other status string, kept verbatim. The text is interned (see
    /// [`intern_status`]) so the enum stays `Copy`.
    Other(&'static str),
}

impl Status {
    /// The kebab-case name used in `state.json` and in `counts`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Held => "held",
            Self::Inflight => "inflight",
            Self::Delivered => "delivered",
            Self::Merging => "merging",
            Self::Merged => "merged",
            Self::Quarantined => "quarantined",
            Self::NoDefect => "no-defect",
            Self::Stuck => "stuck",
            Self::NeedsReplan => "needs-replan",
            Self::Splitting => "splitting",
            Self::Replaced => "replaced",
            Self::Withdrawn => "withdrawn",
            Self::Other(name) => name,
        }
    }

    /// The status named `name`; an unknown name becomes [`Status::Other`].
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "pending" => Self::Pending,
            "held" => Self::Held,
            "inflight" => Self::Inflight,
            "delivered" => Self::Delivered,
            "merging" => Self::Merging,
            "merged" => Self::Merged,
            "quarantined" => Self::Quarantined,
            "no-defect" => Self::NoDefect,
            "stuck" => Self::Stuck,
            "needs-replan" => Self::NeedsReplan,
            "splitting" => Self::Splitting,
            "replaced" => Self::Replaced,
            "withdrawn" => Self::Withdrawn,
            other => Self::Other(intern_status(other)),
        }
    }
}

/// Serde skip predicate: an absent revision is the old shape.
fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// Serde skip predicate: an absent counter is zero.
fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

/// Intern an unknown status string so [`Status`] can stay `Copy`. One leaked
/// allocation per distinct string for the life of the process; the set of
/// hand-set statuses in a state file is tiny.
fn intern_status(name: &str) -> &'static str {
    static NAMES: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());
    let mut names = NAMES.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(known) = names.get(name) {
        return known;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(leaked);
    leaked
}

impl Serialize for Status {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Status {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Ok(Self::from_name(&name))
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
    /// The attempt branch (`unit/<uid>-a<n>`) the hand worked on, kept on the
    /// remote after the attempt ends. `pump.db` keeps it in `unit_events`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The Vivi task handle of the attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

/// One schedulable unit of work.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)] // field names are the state.json keys
pub struct Unit {
    /// The need this unit serves (handle without the `-N` suffix).
    #[serde(default)]
    pub need: String,
    /// Repository target selected by the plan. Legacy state maps to
    /// `default`; a target is fixed once the unit has been attempted.
    #[serde(default = "default_target")]
    pub target: String,
    /// The plan revision that introduced or last amended this unit.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub revision: u64,
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
    /// The unit's kind (`[pump.dispatch.kinds.<name>]`); empty means
    /// `default_kind`.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_dispatch: Option<u64>,
    /// Pool the task was dispatched to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// Pool slot the current/last attempt ran on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    /// Commit produced by the last merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Epoch seconds the unit merged; `pump.db` records this on the merge row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<u64>,
    /// Files the agent touched outside its write scope.
    #[serde(default)]
    pub outside: Vec<String>,
    /// Handles of units that replaced this one.
    #[serde(default)]
    pub replaced_by: Vec<String>,
    /// The agent's verdict text, when it replied with one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict_text: Option<String>,
    /// The unit's unknown-kind exception was raised already.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub kind_reported: bool,
    /// The unit's config-class task-send exception was raised already; a
    /// later successful send clears it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub send_reported: bool,
    /// Consecutive split-planner exits that produced no plan. `pump.db`
    /// keeps it in the unit's `extra_json` column.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub split_fails: u32,
    /// Provider rate-limit (429) split-planner retries; never counts toward
    /// `split_fails`. `pump.db` keeps it in the unit's `extra_json` column.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub split_rate_retries: u32,
    /// Earliest epoch seconds the split planner may respawn for this unit.
    /// `pump.db` keeps it in the unit's `extra_json` column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_retry_at: Option<u64>,
    /// Bad split plans already re-asked; one maximum. `pump.db` keeps it in
    /// the unit's `extra_json` column.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub split_reasks: u32,
    /// Dependency handle holding this unit back, when one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_on: Option<String>,
    /// The unit was requeued once by the blocking-stuck response, so a second
    /// stall parks it stuck again instead of being requeued a second time.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub requeued: bool,
    /// Transient-class failures already retried without counting toward
    /// max_attempts. `pump.db` keeps it in the unit's `extra_json` column.
    #[serde(default)]
    pub transient_retries: u32,
    /// Epoch seconds before which the unit is not redispatched (transient
    /// backoff). `pump.db` keeps it in the unit's `extra_json` column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_not_before: Option<u64>,
    /// The quarantine id this unit is a member of: set on every unit set
    /// aside in a quarantined batch. A repair unit is never a member (see
    /// `repairs`). `pump.db` keeps it in the unit's `extra_json` column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine: Option<String>,
    /// The quarantine id this unit repairs: set only on a repair unit, and
    /// kept apart from `quarantine` so a repair that later meets another
    /// batch still lands or releases its own members. `pump.db` keeps it in
    /// the unit's `extra_json` column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repairs: Option<String>,
    /// Branch to dispatch this unit from instead of `dispatch.from`; a repair
    /// unit starts on its `pump/quarantine/<id>` branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Merge this unit in a batch of its own: set on a repair unit, and on
    /// the quarantined units released when their repair went stuck, found no
    /// defect or could not be opened, so one bad unit cannot sink a batch
    /// again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub solo: bool,
    /// Unknown keys from other pump versions, written back untouched.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Unit {
    /// Target ID, treating programmatically constructed legacy units like
    /// deserialized legacy state (`default`).
    #[must_use]
    pub fn target_id(&self) -> &str {
        if self.target.is_empty() {
            "default"
        } else {
            &self.target
        }
    }
}

fn default_target() -> String {
    String::from("default")
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
    /// The current plan revision; 1 is the first plan.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub revision: u64,
    /// SHA-256 of the thread the current plan was built from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_sha256: Option<String>,
    /// Epoch seconds of the newest thread event already considered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_seen_at: Option<u64>,
    /// A post-plan thread change is waiting for a revision.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub amendment_pending: bool,
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

/// One pool's environment-failure bookkeeping (`env_pauses` entry).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EnvPause {
    /// Epoch seconds of recent host-class failures, kept within the window.
    pub fails: Vec<u64>,
    /// The failing fact or command and its stderr, for the exception.
    pub evidence: String,
    /// Shell command re-run each tick, e.g. `command -v pi`; `None` resumes
    /// on the window passing.
    pub probe: Option<String>,
    /// Epoch second dispatch to this pool paused; `None` while failures are
    /// only counted.
    pub paused_at: Option<u64>,
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
    /// Need handle -> epoch seconds a revision planner started.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub revision_planning: BTreeMap<String, u64>,
    /// Handle -> pid of the board planner currently running.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub planner_pids: BTreeMap<String, u32>,
    /// Handle -> consecutive planner exits without a plan.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub planner_fails: BTreeMap<String, u32>,
    /// Handle -> earliest epoch seconds the planner may relaunch.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub planner_retry_at: BTreeMap<String, u64>,
    /// Handles parked after exhausting planner retries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub planner_failed: Vec<String>,
    /// An attempt record was posted since the last sync push: the next board
    /// pass ships it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sync_pending: bool,
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
    /// The failing test names (after `ignore_file` filtering) main carried at
    /// this tip; `None` for a promotion made before they were recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failing: Option<Vec<String>>,
}

/// The whole pump state: `state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)] // field names are the state.json keys
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
    /// Gate, baseline and promotion state for named repository targets.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub target_states: BTreeMap<String, TargetState>,
    /// Promoted tips, oldest first.
    #[serde(default)]
    pub promotions: Vec<Promotion>,
    /// Pool -> host-class failure bookkeeping and dispatch pause.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env_pauses: BTreeMap<String, EnvPause>,
    /// Unknown keys from other pump versions, written back untouched.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Gate state isolated to one named repository target.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TargetState {
    #[serde(default)]
    pub merges: u64,
    #[serde(default)]
    pub baseline: Option<Vec<String>>,
    #[serde(default)]
    pub last_gate: GateRun,
    #[serde(default)]
    pub gates: Vec<GateRecord>,
    #[serde(default)]
    pub promotions: Vec<Promotion>,
    #[serde(default)]
    pub red: BTreeMap<String, String>,
    #[serde(default)]
    pub red_reported: Vec<String>,
    /// Gate-fix counters and external-gate wait state for this target.
    #[serde(default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Run one target's gate logic against its own state while preserving the
/// workspace-level unit and need graph in the surrounding `State`.
pub fn with_target_state<T>(st: &mut State, target: &str, run: impl FnOnce(&mut State) -> T) -> T {
    if target == "default" {
        return run(st);
    }
    let previous = st.target_states.remove(target).unwrap_or_default();
    let saved = TargetState {
        merges: st.merges,
        baseline: std::mem::replace(&mut st.baseline, previous.baseline),
        last_gate: std::mem::replace(&mut st.last_gate, previous.last_gate),
        gates: std::mem::replace(&mut st.gates, previous.gates),
        promotions: std::mem::replace(&mut st.promotions, previous.promotions),
        red: std::mem::replace(&mut st.red, previous.red),
        red_reported: std::mem::replace(&mut st.red_reported, previous.red_reported),
        extra: std::mem::replace(&mut st.extra, previous.extra),
    };
    st.merges = st
        .units
        .values()
        .filter(|unit| unit.target_id() == target && unit.status == Status::Merged)
        .count() as u64;
    let result = run(st);
    let updated = TargetState {
        merges: st.merges,
        baseline: st.baseline.take(),
        last_gate: std::mem::take(&mut st.last_gate),
        gates: std::mem::take(&mut st.gates),
        promotions: std::mem::take(&mut st.promotions),
        red: std::mem::take(&mut st.red),
        red_reported: std::mem::take(&mut st.red_reported),
        extra: std::mem::take(&mut st.extra),
    };
    st.merges = saved.merges;
    st.baseline = saved.baseline;
    st.last_gate = saved.last_gate;
    st.gates = saved.gates;
    st.promotions = saved.promotions;
    st.red = saved.red;
    st.red_reported = saved.red_reported;
    st.extra = saved.extra;
    st.target_states.insert(target.to_string(), updated);
    result
}

impl State {
    /// Whether any target currently owns an open gate task.
    #[must_use]
    pub fn has_active_gate(&self) -> bool {
        self.last_gate.task.is_some()
            || self
                .target_states
                .values()
                .any(|target| target.last_gate.task.is_some())
    }

    /// Target ID whose gate task is open, if any.
    #[must_use]
    pub fn active_gate_target(&self) -> Option<String> {
        if self.last_gate.task.is_some() {
            return Some(String::from("default"));
        }
        self.target_states
            .iter()
            .find(|(_, target)| target.last_gate.task.is_some())
            .map(|(id, _)| id.clone())
    }
}

/// One unit handed to the merge thread.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct MergeJob {
    /// Unit handle.
    #[serde(default)]
    pub uid: String,
    /// Repository target whose integration clone owns the branch.
    #[serde(default = "default_target")]
    pub target: String,
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
    /// The unit's kind (`[pump.dispatch.kinds.<name>]`); empty means
    /// `default_kind`.
    #[serde(default)]
    pub tools: String,
    /// Files this unit may write.
    #[serde(default)]
    pub write_scope: Vec<String>,
    /// The local repository the branch lives in for a `local` worktree pool
    /// (single-target pumps); `None` means the target's remote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
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
    /// Set (with `ok=null`, `why="quarantined"`) when the unit merged into a
    /// batch whose shared check failed: the batch was pushed aside.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine: Option<Quarantine>,
}

/// A batch set aside after a failed shared check; every merged unit of the
/// batch gets the same value in its result.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Quarantine {
    /// The quarantine id; the batch sits on `pump/quarantine/<id>`.
    #[serde(default)]
    pub id: String,
    /// The shared check that failed.
    #[serde(default)]
    pub cmd: String,
    /// Its exit code.
    #[serde(default)]
    pub code: i32,
    /// Its distilled failure text.
    #[serde(default)]
    pub text: String,
    /// Every unit of the batch that merged cleanly and is set aside.
    #[serde(default)]
    pub units: Vec<String>,
}

impl State {
    /// Load the pump state from `path`.
    ///
    /// A missing file yields the empty state. Units left `merging` by a
    /// crashed run fold back to `delivered`, and an empty `plans_seen` with
    /// known needs is rebuilt as `"<need>.json"` per need — both done
    /// at startup.
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
        state.recover_after_restart();
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

    /// Normalize a state imported from `state.json`: a need or unit written
    /// before revisions existed is revision 1, not 0. Called only on the
    /// import path, so ordinary load/save round-trips keep the old shape.
    pub fn normalize_legacy(&mut self) {
        for record in self.needs.values_mut() {
            if record.revision == 0 {
                record.revision = 1;
            }
        }
        for unit in self.units.values_mut() {
            if unit.revision == 0 {
                unit.revision = 1;
            }
        }
    }

    /// Startup repairs: fold `merging` back to `delivered`, rebuild an empty
    /// `plans_seen` from `needs`.
    pub(crate) fn recover_after_restart(&mut self) {
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
