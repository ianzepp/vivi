//! Durable branch identity and lifecycle for planned needs.
//!
//! Need branch metadata lives under `State.extra["need_branches"]`, which the
//! pump already persists through `pump.db` and `state.json`. Decoding is strict:
//! malformed lifecycle data is an error and can never turn into permission to
//! dispatch against the target branch.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::VivariumError;
use crate::pump::state::{State, Status};

mod state;
pub(crate) use state::backfill_legacy_unit_revisions;
use state::{
    advance_revision_record, clear_freeze, current_unit_ids, encode_error, history_entry,
    new_record, require, set_branch_tip, state_error, unit_fingerprint, write,
};
pub use state::{
    all, binding_for, external_prerequisites, forget, frozen, fully_landed, get, managed,
    managed_unlanded, units_for,
};

mod recover;
pub use recover::{announce_hold, recover_unverified};

mod init;
pub use init::{NeedBranchInitResult, enqueue_init, ensure_creation, reopen};
mod landing;
pub use landing::{
    CleanupRetryState, NeedCleanupReceipt, NeedLandingReceipt, cleanup_retry_state, finish_cleanup,
    record_cleanup_job, record_landing, release_cleanup_job, release_landing, reserve_landing,
};

/// The immutable identity checked by need audits, acceptance, and landing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeedAuditBinding {
    /// Need handle without a message suffix.
    pub need: String,
    /// Repository target that owns the branch.
    pub target: String,
    /// Plan revision whose units were integrated.
    pub revision: u64,
    /// Need branch starting commit on the target integration branch.
    pub base: String,
    /// Frozen need branch commit.
    pub tip: String,
}

impl NeedAuditBinding {
    /// Construct a binding from its durable identity fields.
    #[must_use]
    pub fn new(
        need: impl Into<String>,
        target: impl Into<String>,
        revision: u64,
        base: impl Into<String>,
        tip: impl Into<String>,
    ) -> Self {
        Self {
            need: need.into(),
            target: target.into(),
            revision,
            base: base.into(),
            tip: tip.into(),
        }
    }

    /// Whether this binding has complete identity and full commit IDs.
    #[must_use]
    pub fn validate(&self) -> bool {
        !self.need.is_empty()
            && !self.target.is_empty()
            && self.revision > 0
            && is_full_sha(&self.base)
            && is_full_sha(&self.tip)
    }
}

/// State of one need's isolated branch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NeedBranchStatus {
    /// Branch initialization is queued or being recovered.
    #[default]
    Creating,
    /// Branch exists at the recorded base and can receive unit work.
    Active,
    /// Every current-revision unit is settled and the branch tip is frozen.
    Frozen,
    /// The desired ref exists at an unrelated tip; never overwrite it.
    Collision,
    /// A changed remote ref lacks matching runner ancestry proof; never adopt it.
    Unverified,
    /// Accepted branch inclusion and verified cleanup are both recorded.
    Landed,
}

impl NeedBranchStatus {
    /// The kebab-case word the pump logs and persists for this status.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Active => "active",
            Self::Frozen => "frozen",
            Self::Collision => "collision",
            Self::Unverified => "unverified",
            Self::Landed => "landed",
        }
    }
}

/// Evidence recorded when a current-revision unit is stuck or replaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeedUnitDisposition {
    /// Plan revision to which the disposition applies.
    pub revision: u64,
    /// Explicit terminal state, currently `stuck` or `replaced`.
    pub kind: String,
    /// Failure diagnosis or replacement handles.
    pub detail: String,
}

/// One external prerequisite present when the need branch base was reserved.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NeedBranchPrerequisite {
    /// Exact unit handle referenced by `Unit.after`.
    pub unit: String,
    /// Owning repository target, absent only if the dependency row was missing.
    pub target: Option<String>,
    /// Owning need handle, absent only if the dependency row was missing.
    pub need: Option<String>,
    /// Revision of the dependency need at the child branch base.
    pub revision: u64,
    /// Accepted dependency branch tip, or the legacy unit commit when unmanaged.
    pub tip: Option<String>,
}

/// One branch lifecycle, indexed by target and need in `State.extra`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NeedBranch {
    /// Need handle.
    pub need: String,
    /// Repository target.
    pub target: String,
    /// Current plan revision represented by the branch.
    pub revision: u64,
    /// Branch name in the new `vivi/need/` namespace.
    pub branch: String,
    /// Immutable branch start commit.
    pub base_sha: String,
    /// Target integration tip observed on first branch reservation.
    pub observed_tip_sha: String,
    /// Most recently verified tip of the need branch.
    pub branch_tip_sha: Option<String>,
    /// Trusted source SHA when an unverified publication was observed.
    pub unverified_base_sha: Option<String>,
    /// Remote need-branch tip observed without sufficient runner proof.
    pub observed_unverified_tip_sha: Option<String>,
    /// Terminal merge job associated with the unverified observation.
    pub unverified_job_id: Option<String>,
    /// Explanation of why the observation is held for operator recovery.
    pub unverified_detail: Option<String>,
    /// The pump has already logged this hold, so it logs it once, not every
    /// tick; cleared when the hold ends.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hold_announced: bool,
    /// Frozen tip for the current revision, absent while active.
    pub frozen_tip_sha: Option<String>,
    /// Start of the latest audit range; the first range starts at `base_sha`.
    pub audit_base_sha: String,
    /// Lifecycle state.
    pub status: NeedBranchStatus,
    /// Durable job that currently owns branch initialization.
    pub create_job_id: Option<String>,
    /// Retry generation for branch initialization.
    pub create_attempt: u32,
    /// Last initialization/collision explanation.
    pub error: Option<String>,
    /// Plan revision to which `invalid_receipts` applies.
    pub invalid_receipt_revision: u64,
    /// One `job: reason` entry per consecutive unit merge job whose receipt
    /// failed validation; cleared by the next valid receipt.
    pub invalid_receipts: Vec<String>,
    /// Explicit, revision-bound evidence for stuck or replaced units.
    pub unit_dispositions: BTreeMap<String, NeedUnitDisposition>,
    /// External prerequisites already present in the plan when this base was frozen.
    pub base_prerequisites: BTreeSet<NeedBranchPrerequisite>,
    /// Unit set and plan fingerprint captured by the freeze.
    pub frozen_units: Vec<String>,
    /// Fingerprint of current plan identity and unit terminal commits.
    pub unit_fingerprint: Option<String>,
    /// Prior full need bindings, retained across repair audit rounds.
    pub prior_bindings: Vec<NeedAuditBinding>,
    /// Landing job ID locks this frozen identity until landing fails or cleanup completes.
    pub landing_job_id: Option<String>,
    /// Verified accepted publication on the target integration ref.
    pub landing_receipt: Option<NeedLandingReceipt>,
    /// Cleanup job ID, when a durable pool job owns ref deletion.
    pub cleanup_job_id: Option<String>,
    /// Verified deletion receipt for the need branch.
    pub cleanup_receipt: Option<NeedCleanupReceipt>,
    /// Last landing error, retained for operator diagnosis.
    pub landing_error: Option<String>,
    /// Last cleanup error, retained for operator diagnosis.
    pub cleanup_error: Option<String>,
    /// Whether the last cleanup error was classified as retryable.
    pub cleanup_retryable: bool,
    /// Unknown lifecycle keys from a newer pump; preserved on rewrite.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl NeedBranch {
    /// Whether this record represents a frozen range.
    #[must_use]
    pub fn is_frozen(&self) -> bool {
        self.status == NeedBranchStatus::Frozen
    }

    /// Convert the raw frozen state to its full acceptance binding.
    ///
    /// Callers authorizing audits, acceptance, or landing should use
    /// [`binding_for`], which also verifies that the plan's unit set has not
    /// changed since this record was frozen.
    #[must_use]
    pub fn binding(&self) -> Option<NeedAuditBinding> {
        if !self.is_frozen() {
            return None;
        }
        let binding = NeedAuditBinding::new(
            self.need.clone(),
            self.target.clone(),
            self.revision,
            self.base_sha.clone(),
            self.frozen_tip_sha.clone()?,
        );
        binding.validate().then_some(binding)
    }
}

/// Generate a new per-attempt unit branch.
#[must_use]
pub fn unit_branch(uid: &str, attempt: u32) -> String {
    format!("{}{uid}-a{attempt}", unit_namespace_prefix())
}

/// Root prefix for all newly generated attempt branches.
#[must_use]
pub const fn unit_namespace_prefix() -> &'static str {
    "vivi/unit/"
}

/// Prefix used to parse the attempt suffix from a newly generated unit branch.
#[must_use]
pub fn unit_branch_prefix(uid: &str) -> String {
    format!("{}{uid}-a", unit_namespace_prefix())
}

/// Generate the one branch owned by a need.
#[must_use]
pub fn need_branch(need: &str) -> String {
    format!("vivi/need/{need}")
}

/// Generate a quarantine branch in the new namespace.
#[must_use]
pub fn quarantine_branch(id: &str) -> String {
    format!("{}{id}", quarantine_namespace_prefix())
}

/// Root prefix for newly generated quarantine branches.
#[must_use]
pub const fn quarantine_namespace_prefix() -> &'static str {
    "vivi/quarantine/"
}

/// Remote-tracking ref a new unit starts from while its need branch is active.
#[must_use]
pub fn base_ref(remote: &str, branch: &str) -> String {
    format!("{remote}/{branch}")
}

/// Whether `sha` is a full Git object ID.
#[must_use]
pub fn is_full_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Reserve a new branch from an already-observed target integration tip.
///
/// Existing records retain their original base and branch. A later plan
/// revision invalidates the frozen binding and resumes the same need branch.
pub fn reserve(
    st: &mut State,
    target: &str,
    need: &str,
    revision: u64,
    base_sha: &str,
) -> Result<NeedBranch, VivariumError> {
    if target.is_empty() || need.is_empty() || revision == 0 || !is_full_sha(base_sha) {
        return Err(state_error(
            "reservation identity is incomplete or has an invalid base SHA",
        ));
    }
    if let Some(mut record) = get(st, target, need)? {
        if matches!(
            record.status,
            NeedBranchStatus::Collision | NeedBranchStatus::Unverified
        ) {
            return Err(state_error("need branch is held for collision recovery"));
        }
        if record.status == NeedBranchStatus::Landed && revision > record.revision {
            if record
                .cleanup_receipt
                .as_ref()
                .is_none_or(|receipt| !receipt.verified_absent)
            {
                return Err(state_error(
                    "cannot reuse a landed branch before verified cleanup",
                ));
            }
            let history = history_entry(&record);
            record = new_record(
                target,
                need,
                revision,
                base_sha,
                external_prerequisites(st, target, need, revision)?,
            );
            record.extra.insert(
                "previous_branch".into(),
                serde_json::to_value(history).map_err(encode_error)?,
            );
        } else if revision > record.revision {
            advance_revision_record(&mut record, revision)?;
        }
        return Ok(write(st, record)?);
    }
    let record = new_record(
        target,
        need,
        revision,
        base_sha,
        external_prerequisites(st, target, need, revision)?,
    );
    write(st, record)
}

/// Persist the durable initialization job identity before dispatch.
pub fn record_create_job(
    st: &mut State,
    target: &str,
    need: &str,
    job_id: &str,
) -> Result<(), VivariumError> {
    let mut record = require(st, target, need)?;
    if record.status != NeedBranchStatus::Creating || job_id.is_empty() {
        return Err(state_error(
            "branch initialization job does not match a creating branch",
        ));
    }
    if record
        .create_job_id
        .as_deref()
        .is_some_and(|current| current != job_id)
    {
        return Err(state_error(
            "another branch initialization job already owns this record",
        ));
    }
    record.create_job_id = Some(job_id.to_owned());
    write(st, record)?;
    Ok(())
}

/// Mark branch initialization successful after its worker and runner receipts are verified.
pub fn record_created(
    st: &mut State,
    target: &str,
    need: &str,
    job_id: &str,
    base_sha: &str,
    tip_sha: &str,
) -> Result<(), VivariumError> {
    let mut record = require(st, target, need)?;
    if record.create_job_id.as_deref() != Some(job_id)
        || record.base_sha != base_sha
        || !is_full_sha(tip_sha)
        || tip_sha != base_sha
    {
        return Err(state_error(
            "branch creation receipt does not match its reserved identity",
        ));
    }
    if !matches!(
        record.status,
        NeedBranchStatus::Creating | NeedBranchStatus::Active
    ) {
        return Err(state_error(
            "branch creation receipt arrived after a terminal lifecycle state",
        ));
    }
    record.branch_tip_sha = Some(tip_sha.to_owned());
    record.status = NeedBranchStatus::Active;
    record.error = None;
    write(st, record)?;
    Ok(())
}

/// Record a retryable initialization failure without charging a unit attempt.
pub fn record_creation_failure(
    st: &mut State,
    target: &str,
    need: &str,
    job_id: &str,
    detail: &str,
) -> Result<(), VivariumError> {
    let mut record = require(st, target, need)?;
    if record.create_job_id.as_deref() != Some(job_id)
        || record.status != NeedBranchStatus::Creating
    {
        return Err(state_error(
            "branch initialization failure does not match the active job",
        ));
    }
    record.create_job_id = None;
    record.create_attempt = record.create_attempt.saturating_add(1).max(1);
    record.error = Some(detail.to_owned());
    write(st, record)?;
    Ok(())
}

/// Report an unrelated branch collision without changing or deleting the ref.
pub fn record_collision(
    st: &mut State,
    target: &str,
    need: &str,
    job_id: &str,
    detail: &str,
) -> Result<(), VivariumError> {
    let mut record = require(st, target, need)?;
    if record.create_job_id.as_deref() != Some(job_id)
        || record.status != NeedBranchStatus::Creating
    {
        return Err(state_error(
            "branch collision does not match the active initialization job",
        ));
    }
    record.status = NeedBranchStatus::Collision;
    record.error = Some(detail.to_owned());
    write(st, record)?;
    Ok(())
}

/// Hold a changed remote need ref when its terminal merge lacks runner ancestry proof.
///
/// The observed SHA is diagnostic only. `branch_tip_sha` remains the last
/// verified start point, and ordinary dispatch cannot resume this lifecycle.
pub fn record_unverified_tip(
    st: &mut State,
    target: &str,
    need: &str,
    job_id: &str,
    tip_sha: &str,
    detail: &str,
) -> Result<(), VivariumError> {
    let mut record = require(st, target, need)?;
    let trusted_tip = record
        .branch_tip_sha
        .clone()
        .ok_or_else(|| state_error("unverified publication has no trusted branch tip"))?;
    validate_unverified_observation(&record, job_id, tip_sha, detail, &trusted_tip)?;
    record.unverified_base_sha = Some(trusted_tip);
    record.observed_unverified_tip_sha = Some(tip_sha.to_owned());
    record.unverified_job_id = Some(job_id.to_owned());
    record.unverified_detail = Some(detail.to_owned());
    clear_freeze(&mut record);
    record.status = NeedBranchStatus::Unverified;
    record.error = Some(detail.to_owned());
    write(st, record)?;
    Ok(())
}

fn validate_unverified_observation(
    record: &NeedBranch,
    job_id: &str,
    tip_sha: &str,
    detail: &str,
    trusted_tip: &str,
) -> Result<(), VivariumError> {
    if record.landing_job_id.is_some()
        || !matches!(
            record.status,
            NeedBranchStatus::Active | NeedBranchStatus::Frozen
        )
        || job_id.is_empty()
        || detail.trim().is_empty()
        || !is_full_sha(tip_sha)
        || tip_sha == trusted_tip
    {
        return Err(state_error(
            "unverified need tip does not describe a changed, unlocked active branch",
        ));
    }
    Ok(())
}

/// Advance the current plan revision while retaining the need branch and its original base.
pub fn advance_revision(
    st: &mut State,
    target: &str,
    need: &str,
    revision: u64,
) -> Result<NeedBranch, VivariumError> {
    let mut record = require(st, target, need)?;
    advance_revision_record(&mut record, revision)?;
    write(st, record)
}

/// Record the verified need-branch tip from a merge for the current revision.
pub fn record_tip(
    st: &mut State,
    target: &str,
    need: &str,
    revision: u64,
    base_sha: &str,
    tip_sha: &str,
) -> Result<(), VivariumError> {
    let mut record = require(st, target, need)?;
    if record.landing_job_id.is_some() {
        return Err(state_error("need branch is locked by an active landing"));
    }
    if record.revision != revision
        || record.base_sha != base_sha
        || !is_full_sha(tip_sha)
        || !matches!(
            record.status,
            NeedBranchStatus::Active | NeedBranchStatus::Frozen
        )
    {
        return Err(state_error(
            "need branch tip does not match the current revision/base",
        ));
    }
    set_branch_tip(&mut record, tip_sha);
    write(st, record)?;
    Ok(())
}

/// Record a branch tip only after runner publication and ancestry have been verified.
///
/// This changes trusted branch truth and invalidates any freeze. It does not
/// accept an old job's revision as current; callers still reject stale results.
pub fn observe_branch_tip(
    st: &mut State,
    target: &str,
    need: &str,
    tip_sha: &str,
) -> Result<(), VivariumError> {
    let mut record = require(st, target, need)?;
    if record.landing_job_id.is_some() {
        return Err(state_error("need branch is locked by an active landing"));
    }
    if !is_full_sha(tip_sha)
        || !matches!(
            record.status,
            NeedBranchStatus::Active | NeedBranchStatus::Frozen
        )
    {
        return Err(state_error(
            "observed need branch tip is invalid or the branch is not active",
        ));
    }
    set_branch_tip(&mut record, tip_sha);
    write(st, record)?;
    Ok(())
}

/// Record a revision-bound disposition for a Stuck or Replaced current unit.
pub fn record_disposition(
    st: &mut State,
    target: &str,
    need: &str,
    unit: &str,
    revision: u64,
    kind: &str,
    detail: &str,
) -> Result<(), VivariumError> {
    if !matches!(kind, "stuck" | "replaced") {
        return Err(state_error(
            "unit disposition kind must be stuck or replaced",
        ));
    }
    let mut record = require(st, target, need)?;
    if revision != record.revision || record.landing_job_id.is_some() {
        return Err(state_error(
            "unit disposition is stale or the branch is locked by landing",
        ));
    }
    if !st.units.get(unit).is_some_and(|row| {
        row.need == need && row.target_id() == target && row.revision == revision
    }) {
        return Err(state_error("unit does not belong to this target and need"));
    }
    record.unit_dispositions.insert(
        unit.to_owned(),
        NeedUnitDisposition {
            revision,
            kind: kind.to_owned(),
            detail: detail.to_owned(),
        },
    );
    write(st, record)?;
    Ok(())
}

/// Consecutive invalid unit merge receipts after which dispatch stops.
pub const INVALID_RECEIPT_LIMIT: usize = 3;

/// Record one more invalid unit merge receipt for this plan revision and return
/// the consecutive entries so far, oldest first.
pub fn record_invalid_receipt(
    st: &mut State,
    target: &str,
    need: &str,
    revision: u64,
    entry: &str,
) -> Result<Vec<String>, VivariumError> {
    let mut record = require(st, target, need)?;
    if record.invalid_receipt_revision != revision {
        record.invalid_receipt_revision = revision;
        record.invalid_receipts.clear();
    }
    record.invalid_receipts.push(entry.to_owned());
    let entries = record.invalid_receipts.clone();
    write(st, record)?;
    Ok(entries)
}

/// End the invalid-receipt streak after a valid receipt.
pub fn clear_invalid_receipts(
    st: &mut State,
    target: &str,
    need: &str,
) -> Result<(), VivariumError> {
    let Some(mut record) = get(st, target, need)? else {
        return Ok(());
    };
    if record.invalid_receipts.is_empty() {
        return Ok(());
    }
    record.invalid_receipts.clear();
    write(st, record)?;
    Ok(())
}

impl NeedBranch {
    /// Whether unit merges for this plan revision are held after repeated
    /// invalid receipts; a new plan revision releases the hold.
    #[must_use]
    pub fn receipts_held(&self, revision: u64) -> bool {
        self.invalid_receipt_revision == revision
            && self.invalid_receipts.len() >= INVALID_RECEIPT_LIMIT
    }
}

/// Whether every unit in this plan is terminally merged or explicitly no-defect.
pub fn current_units_settled(
    st: &State,
    target: &str,
    need: &str,
    revision: u64,
) -> Result<bool, VivariumError> {
    if st
        .needs
        .get(need)
        .is_some_and(|record| record.revision > 0 && record.revision != revision)
    {
        return Ok(false);
    }
    let Some(listed) = current_plan_unit_ids(st, target, need, revision)? else {
        return Ok(false);
    };
    for uid in listed {
        let Some(unit) = st.units.get(&uid) else {
            return Ok(false);
        };
        if !matches!(unit.status, Status::Merged | Status::NoDefect) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn current_plan_unit_ids(
    st: &State,
    target: &str,
    need: &str,
    revision: u64,
) -> Result<Option<BTreeSet<String>>, VivariumError> {
    let Some(plan) = st.needs.get(need) else {
        return Ok(Some(
            current_unit_ids(st, target, need, revision)
                .into_iter()
                .collect(),
        ));
    };
    let mut current = BTreeSet::new();
    let mut has_listed_target_units = false;
    for uid in &plan.units {
        let Some(unit) = st.units.get(uid) else {
            return Ok(None);
        };
        if unit.need != need || unit.target_id() != target {
            continue;
        }
        has_listed_target_units = true;
        if unit.revision < revision {
            continue;
        }
        if unit.revision > revision || !matches!(unit.status, Status::Merged | Status::NoDefect) {
            return Ok(None);
        }
        current.insert(uid.clone());
    }
    if current.is_empty() && has_listed_target_units {
        return Ok(None);
    }
    Ok(live_current_units(st, target, need, revision)
        .eq(&current)
        .then_some(current))
}

fn live_current_units(st: &State, target: &str, need: &str, revision: u64) -> BTreeSet<String> {
    st.units
        .iter()
        .filter(|(_, unit)| {
            unit.need == need && unit.target_id() == target && unit.revision == revision
        })
        .map(|(uid, _)| uid.clone())
        .collect()
}

/// Start a narrow repair audit range at the previous frozen tip.
pub fn begin_repair(
    st: &mut State,
    target: &str,
    need: &str,
    revision: u64,
) -> Result<NeedBranch, VivariumError> {
    let mut record = require(st, target, need)?;
    if record.landing_job_id.is_some() || !record.is_frozen() {
        return Err(state_error(
            "repair requires an unlocked frozen need branch",
        ));
    }
    if revision == 0 || revision < record.revision {
        return Err(state_error(
            "repair revision is invalid or older than the branch revision",
        ));
    }
    let previous = record
        .binding()
        .ok_or_else(|| state_error("frozen branch has no valid full binding"))?;
    record.prior_bindings.push(previous.clone());
    record.audit_base_sha = previous.tip;
    record.revision = revision;
    record.frozen_tip_sha = None;
    record.frozen_units.clear();
    record.unit_fingerprint = None;
    record.status = NeedBranchStatus::Active;
    write(st, record)
}

/// Freeze the current, settled branch tip and return its full acceptance binding.
pub fn freeze(
    st: &mut State,
    target: &str,
    need: &str,
    revision: u64,
    tip_sha: &str,
) -> Result<NeedAuditBinding, VivariumError> {
    let mut record = require(st, target, need)?;
    if record.status != NeedBranchStatus::Active
        || record.landing_job_id.is_some()
        || record.revision != revision
        || !is_full_sha(tip_sha)
        || record.branch_tip_sha.as_deref() != Some(tip_sha)
    {
        return Err(state_error(
            "freeze does not match an active branch identity and tip",
        ));
    }
    if !current_units_settled(st, target, need, revision)? {
        return Err(state_error(
            "cannot freeze while current-revision units remain unsettled",
        ));
    }
    record.frozen_units = current_unit_ids(st, target, need, revision);
    record.unit_fingerprint = Some(unit_fingerprint(st, target, need, revision));
    record.frozen_tip_sha = Some(tip_sha.to_owned());
    record.status = NeedBranchStatus::Frozen;
    let binding = record
        .binding()
        .ok_or_else(|| state_error("could not construct a valid frozen binding"))?;
    write(st, record)?;
    Ok(binding)
}

#[cfg(test)]
#[path = "need_test.rs"]
mod tests;
