//! Durable typed state accessors and unit-plan snapshots for need branches.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::error::VivariumError;
use crate::pump::state::{State, Status};

use super::{
    NeedAuditBinding, NeedBranch, NeedBranchPrerequisite, NeedBranchStatus, current_units_settled,
    is_full_sha, need_branch,
};

const STATE_KEY: &str = "need_branches";

/// Read one lifecycle record, failing visibly on malformed persisted state.
pub fn get(st: &State, target: &str, need: &str) -> Result<Option<NeedBranch>, VivariumError> {
    let Some(root) = st.extra.get(STATE_KEY) else {
        return Ok(None);
    };
    let root = root
        .as_object()
        .ok_or_else(|| state_error("root is not an object"))?;
    let Some(target_value) = root.get(target) else {
        return Ok(None);
    };
    let target_map = target_value
        .as_object()
        .ok_or_else(|| state_error(&format!("target {target:?} is not an object")))?;
    let Some(value) = target_map.get(need) else {
        return Ok(None);
    };
    decode_record(value.clone(), target, need).map(Some)
}

/// Read every lifecycle record in deterministic target/need order.
pub fn all(st: &State) -> Result<Vec<NeedBranch>, VivariumError> {
    let Some(root) = st.extra.get(STATE_KEY) else {
        return Ok(Vec::new());
    };
    let root = root
        .as_object()
        .ok_or_else(|| state_error("root is not an object"))?;
    let mut records = Vec::new();
    for (target, target_value) in root {
        let target_map = target_value
            .as_object()
            .ok_or_else(|| state_error(&format!("target {target:?} is not an object")))?;
        for (need, value) in target_map {
            records.push(decode_record(value.clone(), target, need)?);
        }
    }
    Ok(records)
}

/// Read records whose frozen identity still matches the plan and unit state.
pub fn frozen(st: &State) -> Result<Vec<NeedBranch>, VivariumError> {
    let mut result = Vec::new();
    for record in all(st)? {
        if binding_for(st, &record.target, &record.need)?.is_some() {
            result.push(record);
        }
    }
    Ok(result)
}

/// Whether any target of `need` has entered managed branch ownership.
pub fn managed(st: &State, need: &str) -> Result<bool, VivariumError> {
    Ok(all(st)?.iter().any(|record| record.need == need))
}

/// Whether a managed need still has a branch that is not landed and cleaned.
pub fn managed_unlanded(st: &State, need: &str) -> Result<bool, VivariumError> {
    let records = all(st)?;
    let current = current_revision(st, need);
    Ok(records.iter().any(|record| {
        record.need == need
            && (record.status != NeedBranchStatus::Landed || record.revision != current)
    }))
}

/// Whether all targets owning units of `need` have verified landing and cleanup.
pub fn fully_landed(st: &State, need: &str) -> Result<bool, VivariumError> {
    let targets: BTreeSet<String> = targets_for_need(st, need)
        .into_iter()
        .chain(
            all(st)?
                .into_iter()
                .filter(|record| record.need == need)
                .map(|record| record.target),
        )
        .collect();
    if targets.is_empty() {
        return Ok(false);
    }
    for target in targets {
        let Some(record) = get(st, &target, need)? else {
            return Ok(false);
        };
        if record.status != NeedBranchStatus::Landed
            || record.revision != current_revision(st, need)
            || record.landing_receipt.is_none()
            || record.cleanup_receipt.is_none()
            || st
                .needs
                .get(need)
                .is_some_and(|plan| plan.amendment_pending)
            || record.frozen_units != current_unit_ids(st, &target, need, record.revision)
            || !current_units_settled(st, &target, need, record.revision)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Enumerate all units associated with one target and need.
#[must_use]
pub fn units_for(st: &State, target: &str, need: &str) -> Vec<String> {
    let mut units = BTreeSet::new();
    if let Some(record) = st.needs.get(need) {
        for uid in &record.units {
            if st
                .units
                .get(uid)
                .is_some_and(|unit| unit.need == need && unit.target_id() == target)
            {
                units.insert(uid.clone());
            }
        }
    }
    for (uid, unit) in &st.units {
        if unit.need == need && unit.target_id() == target {
            units.insert(uid.clone());
        }
    }
    units.into_iter().collect()
}

/// Current external unit prerequisites on a target, including their revision/tip identity.
pub fn external_prerequisites(
    st: &State,
    target: &str,
    need: &str,
    revision: u64,
) -> Result<BTreeSet<NeedBranchPrerequisite>, VivariumError> {
    let mut prerequisites = BTreeSet::new();
    for uid in current_unit_ids(st, target, need, revision) {
        if let Some(unit) = st.units.get(&uid) {
            collect_prerequisites(st, unit, target, need, &mut prerequisites)?;
        }
    }
    Ok(prerequisites)
}

fn collect_prerequisites(
    st: &State,
    unit: &crate::pump::state::Unit,
    target: &str,
    need: &str,
    output: &mut BTreeSet<NeedBranchPrerequisite>,
) -> Result<(), VivariumError> {
    for dependency in &unit.after {
        let Some(dep) = st.units.get(dependency) else {
            output.insert(missing_prerequisite(dependency));
            continue;
        };
        if dep.need == need && dep.target_id() == target {
            continue;
        }
        output.insert(prerequisite_identity(st, dependency, dep)?);
    }
    Ok(())
}

fn prerequisite_identity(
    st: &State,
    unit_id: &str,
    unit: &crate::pump::state::Unit,
) -> Result<NeedBranchPrerequisite, VivariumError> {
    let target = unit.target_id();
    let need = unit.need.as_str();
    let branch = get(st, target, need)?;
    let accepted = branch.as_ref().and_then(|record| {
        record
            .landing_receipt
            .as_ref()
            .map(|receipt| &receipt.binding)
    });
    let revision = accepted.map_or_else(
        || {
            st.needs
                .get(need)
                .map_or(unit.revision, |record| record.revision.max(1))
        },
        |binding| binding.revision,
    );
    let tip = accepted
        .map(|binding| binding.tip.clone())
        .or_else(|| unit.commit.clone());
    Ok(NeedBranchPrerequisite {
        unit: unit_id.to_owned(),
        target: Some(target.to_owned()),
        need: Some(need.to_owned()),
        revision,
        tip,
    })
}

fn missing_prerequisite(unit: &str) -> NeedBranchPrerequisite {
    NeedBranchPrerequisite {
        unit: unit.to_owned(),
        target: None,
        need: None,
        revision: 0,
        tip: None,
    }
}

/// Backfill zero revisions only for unit IDs listed in their current need plan.
pub(crate) fn backfill_legacy_unit_revisions(st: &mut State) -> usize {
    let plans: Vec<(String, u64, Vec<String>)> = st
        .needs
        .iter()
        .map(|(need, record)| (need.clone(), record.revision, record.units.clone()))
        .collect();
    let mut changed = 0;
    for (need, old_revision, units) in plans {
        let open_units = open_legacy_units(st, &need, &units);
        let revision = if old_revision == 0 && !open_units.is_empty() {
            1
        } else {
            old_revision
        };
        if let Some(record) = st.needs.get_mut(&need) {
            record.revision = revision;
        }
        for uid in open_units {
            if let Some(unit) = st.units.get_mut(&uid)
                && revision > 0
            {
                unit.revision = revision;
                changed += 1;
            }
        }
    }
    changed
}

fn open_legacy_units(st: &State, need: &str, units: &[String]) -> Vec<String> {
    units
        .iter()
        .filter(|uid| {
            st.units.get(*uid).is_some_and(|unit| {
                unit.need == need && unit.revision == 0 && !legacy_unit_satisfied(st, uid, 0)
            })
        })
        .cloned()
        .collect()
}

fn legacy_unit_satisfied(st: &State, uid: &str, depth: usize) -> bool {
    let Some(unit) = st.units.get(uid) else {
        return false;
    };
    match unit.status {
        Status::Merged | Status::NoDefect => true,
        Status::Replaced if depth < 6 => {
            !unit.replaced_by.is_empty()
                && unit
                    .replaced_by
                    .iter()
                    .all(|next| legacy_unit_satisfied(st, next, depth + 1))
        }
        _ => false,
    }
}

/// Return the full binding only if plan revision, unit set, and terminal state still match.
pub fn binding_for(
    st: &State,
    target: &str,
    need: &str,
) -> Result<Option<NeedAuditBinding>, VivariumError> {
    let Some(record) = get(st, target, need)? else {
        return Ok(None);
    };
    let Some(binding) = record.binding() else {
        return Ok(None);
    };
    if st
        .needs
        .get(need)
        .is_some_and(|plan| plan.amendment_pending)
        || current_revision(st, need) != record.revision
        || record.frozen_units != current_unit_ids(st, target, need, record.revision)
        || record.unit_fingerprint.as_deref()
            != Some(unit_fingerprint(st, target, need, record.revision).as_str())
        || record.base_prerequisites != external_prerequisites(st, target, need, record.revision)?
        || !current_units_settled(st, target, need, record.revision)?
    {
        return Ok(None);
    }
    Ok(Some(binding))
}

pub(super) fn write(st: &mut State, record: NeedBranch) -> Result<NeedBranch, VivariumError> {
    let root = branch_root(st)?;
    let target_value = root
        .entry(record.target.clone())
        .or_insert_with(|| Value::Object(Map::new()));
    let target_map = target_value
        .as_object_mut()
        .ok_or_else(|| state_error(&format!("target {:?} is not an object", record.target)))?;
    target_map.insert(
        record.need.clone(),
        serde_json::to_value(&record).map_err(encode_error)?,
    );
    Ok(record)
}

pub(super) fn require(st: &State, target: &str, need: &str) -> Result<NeedBranch, VivariumError> {
    get(st, target, need)?
        .ok_or_else(|| state_error(&format!("no branch lifecycle for {target}/{need}")))
}

pub(super) fn state_error(message: &str) -> VivariumError {
    VivariumError::Message(format!("need branch state: {message}"))
}

pub(super) fn encode_error(error: serde_json::Error) -> VivariumError {
    state_error(&format!("cannot encode lifecycle state: {error}"))
}

fn branch_root(st: &mut State) -> Result<&mut Map<String, Value>, VivariumError> {
    st.extra
        .entry(STATE_KEY.to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| state_error("root is not an object"))
}

fn decode_record(value: Value, target: &str, need: &str) -> Result<NeedBranch, VivariumError> {
    let record: NeedBranch = serde_json::from_value(value)
        .map_err(|err| state_error(&format!("cannot decode {target}/{need} lifecycle: {err}")))?;
    validate_identity(&record, target, need)?;
    Ok(record)
}

fn validate_identity(record: &NeedBranch, target: &str, need: &str) -> Result<(), VivariumError> {
    validate_record_key(record, target, need)?;
    validate_record_base(record, target, need)?;
    validate_record_tips(record, target, need)?;
    validate_record_status(record, target, need)?;
    validate_record_receipts(record, target, need)
}

fn validate_record_key(record: &NeedBranch, target: &str, need: &str) -> Result<(), VivariumError> {
    if record.target == target && record.need == need && record.branch == need_branch(need) {
        Ok(())
    } else {
        Err(state_error(&format!(
            "{target}/{need} lifecycle identity is inconsistent"
        )))
    }
}

fn validate_record_base(
    record: &NeedBranch,
    target: &str,
    need: &str,
) -> Result<(), VivariumError> {
    if record.revision > 0
        && record.create_attempt > 0
        && is_full_sha(&record.base_sha)
        && is_full_sha(&record.observed_tip_sha)
        && is_full_sha(&record.audit_base_sha)
    {
        Ok(())
    } else {
        Err(state_error(&format!(
            "{target}/{need} lifecycle has incomplete identity or invalid base SHA"
        )))
    }
}

fn validate_record_tips(
    record: &NeedBranch,
    target: &str,
    need: &str,
) -> Result<(), VivariumError> {
    let valid = record.branch_tip_sha.as_deref().is_none_or(is_full_sha)
        && record.frozen_tip_sha.as_deref().is_none_or(is_full_sha)
        && record
            .unverified_base_sha
            .as_deref()
            .is_none_or(is_full_sha)
        && record
            .observed_unverified_tip_sha
            .as_deref()
            .is_none_or(is_full_sha);
    if valid {
        Ok(())
    } else {
        Err(state_error(&format!(
            "{target}/{need} lifecycle has invalid branch tip SHA"
        )))
    }
}

fn validate_record_status(
    record: &NeedBranch,
    target: &str,
    need: &str,
) -> Result<(), VivariumError> {
    match record.status {
        NeedBranchStatus::Creating
            if record.branch_tip_sha.is_none()
                && record.frozen_tip_sha.is_none()
                && no_unverified_hold(record) =>
        {
            Ok(())
        }
        NeedBranchStatus::Active
            if record.branch_tip_sha.is_some()
                && record.frozen_tip_sha.is_none()
                && record.frozen_units.is_empty()
                && record.unit_fingerprint.is_none()
                && no_unverified_hold(record) =>
        {
            Ok(())
        }
        NeedBranchStatus::Frozen
            if record.branch_tip_sha.as_deref() == record.frozen_tip_sha.as_deref()
                && record
                    .unit_fingerprint
                    .as_deref()
                    .is_some_and(valid_fingerprint)
                && no_unverified_hold(record) =>
        {
            Ok(())
        }
        NeedBranchStatus::Collision
            if record
                .error
                .as_deref()
                .is_some_and(|error| !error.is_empty())
                && no_unverified_hold(record) =>
        {
            Ok(())
        }
        NeedBranchStatus::Unverified if valid_unverified_hold(record) => Ok(()),
        NeedBranchStatus::Landed
            if record.landing_job_id.is_some()
                && record.cleanup_job_id.is_some()
                && record.landing_receipt.is_some()
                && record
                    .cleanup_receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt.verified_absent)
                && no_unverified_hold(record) =>
        {
            Ok(())
        }
        _ => Err(state_error(&format!(
            "{target}/{need} lifecycle status has incomplete state"
        ))),
    }
}

fn valid_unverified_hold(record: &NeedBranch) -> bool {
    record.branch_tip_sha.as_deref() == record.unverified_base_sha.as_deref()
        && record.branch_tip_sha.is_some()
        && record
            .observed_unverified_tip_sha
            .as_deref()
            .is_some_and(|tip| record.unverified_base_sha.as_deref() != Some(tip))
        && record
            .unverified_job_id
            .as_deref()
            .is_some_and(|job| !job.is_empty())
        && record
            .unverified_detail
            .as_deref()
            .is_some_and(|detail| !detail.trim().is_empty())
        && record.frozen_tip_sha.is_none()
        && record.frozen_units.is_empty()
        && record.unit_fingerprint.is_none()
        && record.landing_job_id.is_none()
        && record.landing_receipt.is_none()
        && record.cleanup_job_id.is_none()
        && record.cleanup_receipt.is_none()
}

fn no_unverified_hold(record: &NeedBranch) -> bool {
    record.unverified_base_sha.is_none()
        && record.observed_unverified_tip_sha.is_none()
        && record.unverified_job_id.is_none()
        && record.unverified_detail.is_none()
}

fn validate_record_receipts(
    record: &NeedBranch,
    target: &str,
    need: &str,
) -> Result<(), VivariumError> {
    validate_landing_receipt(record, target, need)?;
    validate_cleanup_receipt(record, target, need)
}

fn validate_landing_receipt(
    record: &NeedBranch,
    target: &str,
    need: &str,
) -> Result<(), VivariumError> {
    let Some(receipt) = &record.landing_receipt else {
        return Ok(());
    };
    let valid = recorded_frozen_binding(record).as_ref() == Some(&receipt.binding)
        && record.landing_job_id.as_deref() == Some(receipt.landing_job_id.as_str())
        && is_full_sha(&receipt.candidate_sha)
        && receipt.runner_published_sha == receipt.candidate_sha
        && is_full_sha(&receipt.observed_target_sha)
        && receipt.accepted_tip_in_candidate
        && receipt.candidate_in_observed_target
        && !receipt.acceptance_receipt_key.is_empty()
        && !receipt.acceptance_job_id.is_empty()
        && matches!(receipt.verdict.as_str(), "clean_pass" | "residual")
        && !receipt.target_ref.is_empty();
    if valid {
        Ok(())
    } else {
        Err(state_error(&format!(
            "{target}/{need} landing receipt is inconsistent"
        )))
    }
}

fn recorded_frozen_binding(record: &NeedBranch) -> Option<NeedAuditBinding> {
    let binding = NeedAuditBinding::new(
        record.need.clone(),
        record.target.clone(),
        record.revision,
        record.base_sha.clone(),
        record.frozen_tip_sha.clone()?,
    );
    binding.validate().then_some(binding)
}

fn validate_cleanup_receipt(
    record: &NeedBranch,
    target: &str,
    need: &str,
) -> Result<(), VivariumError> {
    let Some(receipt) = &record.cleanup_receipt else {
        return Ok(());
    };
    let valid = receipt.branch_ref == format!("refs/heads/{}", record.branch)
        && receipt.expected_sha == record.frozen_tip_sha.as_deref().unwrap_or_default()
        && receipt.verified_absent
        && matches!(receipt.runner_state.as_str(), "deleted" | "already-absent")
        && (receipt.runner_state != "deleted"
            || receipt.before_sha.as_deref() == Some(receipt.expected_sha.as_str()));
    if valid {
        Ok(())
    } else {
        Err(state_error(&format!(
            "{target}/{need} cleanup receipt is inconsistent"
        )))
    }
}

fn valid_fingerprint(fingerprint: &str) -> bool {
    fingerprint.len() == 64 && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn current_revision(st: &State, need: &str) -> u64 {
    st.needs
        .get(need)
        .map(|record| record.revision)
        .filter(|revision| *revision > 0)
        .or_else(|| {
            st.units
                .values()
                .filter(|unit| unit.need == need)
                .map(|unit| unit.revision)
                .max()
        })
        .unwrap_or(1)
}

pub(super) fn unit_fingerprint(st: &State, target: &str, need: &str, revision: u64) -> String {
    let units: Vec<Value> = current_unit_ids(st, target, need, revision)
        .iter()
        .map(|uid| {
            let unit = st.units.get(uid);
            json!({
                "id": uid,
                "revision": unit.map(|unit| unit.revision),
                "status": unit.map(|unit| unit.status.as_str()),
                "commit": unit.and_then(|unit| unit.commit.as_deref()),
                "title": unit.map(|unit| &unit.title),
                "brief": unit.map(|unit| &unit.brief),
                "write_scope": unit.map(|unit| &unit.write_scope),
                "proof": unit.map(|unit| &unit.proof),
                "tools": unit.map(|unit| &unit.tools),
                "replaced_by": unit.map(|unit| &unit.replaced_by),
                "after": unit.map(|unit| &unit.after),
            })
        })
        .collect();
    let bytes = serde_json::to_vec(&json!({
        "need_revision": current_revision(st, need),
        "revision": revision,
        "need_verdict": st.needs.get(need).map(|record| &record.verdict),
        "need_reason": st.needs.get(need).map(|record| &record.reason),
        "need_units": st.needs.get(need).map(|record| &record.units),
        "need_thread": st.needs.get(need).map(|record| &record.thread_sha256),
        "target_units": units,
    }))
    .unwrap_or_default();
    hex::encode(Sha256::digest(bytes))
}

pub(super) fn current_unit_ids(st: &State, target: &str, need: &str, revision: u64) -> Vec<String> {
    let mut units = BTreeSet::new();
    let listed = st.needs.get(need).map(|plan| &plan.units);
    if let Some(listed) = listed {
        for uid in listed {
            if st.units.get(uid).is_some_and(|unit| {
                unit.need == need && unit.target_id() == target && unit.revision == revision
            }) {
                units.insert(uid.clone());
            }
        }
    } else {
        for (uid, unit) in &st.units {
            if unit.need == need && unit.target_id() == target && unit.revision == revision {
                units.insert(uid.clone());
            }
        }
    }
    units.into_iter().collect()
}

fn targets_for_need(st: &State, need: &str) -> BTreeSet<String> {
    st.units
        .values()
        .filter(|unit| unit.need == need)
        .map(|unit| unit.target_id().to_owned())
        .collect()
}

pub(super) fn history_entry(record: &NeedBranch) -> Value {
    json!({
        "need": record.need,
        "target": record.target,
        "revision": record.revision,
        "branch": record.branch,
        "base_sha": record.base_sha,
        "base_prerequisites": record.base_prerequisites,
        "frozen_tip_sha": record.frozen_tip_sha,
        "landing_receipt": record.landing_receipt,
        "cleanup_receipt": record.cleanup_receipt,
    })
}

pub(super) fn new_record(
    target: &str,
    need: &str,
    revision: u64,
    base_sha: &str,
    base_prerequisites: BTreeSet<NeedBranchPrerequisite>,
) -> NeedBranch {
    NeedBranch {
        need: need.to_owned(),
        target: target.to_owned(),
        revision,
        branch: need_branch(need),
        base_sha: base_sha.to_owned(),
        observed_tip_sha: base_sha.to_owned(),
        audit_base_sha: base_sha.to_owned(),
        create_attempt: 1,
        base_prerequisites,
        ..NeedBranch::default()
    }
}

pub(super) fn advance_revision_record(
    record: &mut NeedBranch,
    revision: u64,
) -> Result<(), VivariumError> {
    validate_revision_advance(record, revision)?;
    if revision == record.revision {
        return Ok(());
    }
    if let Some(binding) = record.binding() {
        record.prior_bindings.push(binding);
    }
    record.revision = revision;
    record.frozen_tip_sha = None;
    record.frozen_units.clear();
    record.unit_fingerprint = None;
    record.unit_dispositions.clear();
    record.status = match record.status {
        NeedBranchStatus::Creating => NeedBranchStatus::Creating,
        NeedBranchStatus::Collision => NeedBranchStatus::Collision,
        NeedBranchStatus::Unverified => NeedBranchStatus::Unverified,
        _ => NeedBranchStatus::Active,
    };
    if !matches!(
        record.status,
        NeedBranchStatus::Collision | NeedBranchStatus::Unverified
    ) {
        record.error = None;
    }
    Ok(())
}

fn validate_revision_advance(record: &NeedBranch, revision: u64) -> Result<(), VivariumError> {
    if revision == 0 || revision < record.revision {
        return Err(state_error("plan revision must increase monotonically"));
    }
    if record.landing_job_id.is_some() {
        return Err(state_error("need branch is locked by an active landing"));
    }
    if record.status == NeedBranchStatus::Landed && revision != record.revision {
        return Err(state_error(
            "landed need branch must be reserved as a new lifecycle",
        ));
    }
    Ok(())
}

pub(super) fn set_branch_tip(record: &mut NeedBranch, tip_sha: &str) {
    if record.branch_tip_sha.as_deref() != Some(tip_sha) {
        record.frozen_tip_sha = None;
        record.frozen_units.clear();
        record.unit_fingerprint = None;
        if record.status != NeedBranchStatus::Creating {
            record.status = NeedBranchStatus::Active;
        }
    }
    record.branch_tip_sha = Some(tip_sha.to_owned());
}

pub(super) fn clear_freeze(record: &mut NeedBranch) {
    record.frozen_tip_sha = None;
    record.frozen_units.clear();
    record.unit_fingerprint = None;
}
