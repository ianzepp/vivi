//! Durable routing for frozen need-audit receipts.
//!
//! Verdict state lives in `State.extra` so pump.db needs no schema migration.
//! External needs use deterministic subjects and are looked up before send;
//! repair units and verdict state are written together by the pump savepoint.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::VivariumError;
use crate::pump::host::Ctx;
use crate::pump::need::{self, NeedAuditBinding};
use crate::pump::need_audit::{self, NeedAuditReceipt, NeedAuditVerdict};
use crate::pump::state::State;

/// The `state.extra` entry for per-need verdict lifecycles.
pub const EXTRA_KEY: &str = "need_verdicts";

/// A receipt accepted for landing, together with its durable audit job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedNeedReceipt {
    /// The validated audit report and its frozen range.
    pub receipt: NeedAuditReceipt,
    /// Stable pump job that carried the audit assignment.
    pub job_id: String,
}

/// Durable escalation attached to a need branch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeedAuditHold {
    /// Why the branch is held.
    pub reason: String,
    /// Audit receipt that established the hold.
    pub receipt_key: String,
    /// Mailspace need sent to the operator and head-cto.
    pub escalation_need: String,
    /// Paths or findings that remain blocked.
    pub boundary: Vec<String>,
}

/// Original or repair range provenance retained across every audit round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeedAuditRange {
    /// Full need range whose current tip is being considered.
    pub binding: NeedAuditBinding,
    /// Exact diff start read by the auditor in this round.
    pub audit_base: String,
    /// Round number from the auditor's durable job.
    pub round: u32,
    /// Stable receipt identity.
    pub receipt_key: String,
    /// Full report provenance.
    pub report_reference: String,
}

/// Pending repair-only audit request, exposed to the audit dispatcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedRepairAuditRequest {
    /// Full frozen branch binding used for acceptance.
    pub binding: NeedAuditBinding,
    /// Review starts at the previous frozen tip, so only the repair diff is read.
    pub audit_base: String,
    /// Current round, bounded by the auditor charter.
    pub round: u32,
    /// Prior blocked findings and provenance for the auditor's brief.
    pub context: String,
}

/// All routing state for one target and need.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeedVerdictState {
    /// Original frozen range, retained even after repair revisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_binding: Option<NeedAuditBinding>,
    /// Latest accepted clean or residual receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted: Option<AcceptedNeedReceipt>,
    /// A durable ship hold is sticky until an operator resolves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<NeedAuditHold>,
    /// Full original and repair review ranges.
    #[serde(default)]
    pub ranges: Vec<NeedAuditRange>,
    /// Exact report receipts already routed.
    #[serde(default)]
    pub receipts: BTreeMap<String, NeedAuditReceipt>,
    /// Residual need handle by deterministic receipt/finding key.
    #[serde(default)]
    pub residual_needs: BTreeMap<String, String>,
    /// Latest repair unit by revision and finding ID.
    #[serde(default)]
    pub repair_units: BTreeMap<String, String>,
    /// Repair units minted per finding key; escalation uses `[pump.need].repair_rounds`.
    #[serde(default)]
    pub repair_counts: BTreeMap<String, u32>,
    /// Current repair-only review start and its next durable round.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recheck_base: Option<String>,
    #[serde(default)]
    pub recheck_round: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recheck_context: Option<String>,
    /// Durable audit job already queued for the current repair diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recheck_job_id: Option<String>,
}

/// Read one need's verdict state; malformed state is an error, not a reset.
pub fn state(
    st: &State,
    target: &str,
    need_handle: &str,
) -> Result<NeedVerdictState, VivariumError> {
    let Some(root) = st.extra.get(EXTRA_KEY) else {
        return Ok(NeedVerdictState::default());
    };
    let targets = root.as_object().ok_or_else(|| {
        VivariumError::Message(String::from("need verdict state root is not an object"))
    })?;
    let Some(target_state) = targets.get(target) else {
        return Ok(NeedVerdictState::default());
    };
    let needs = target_state.as_object().ok_or_else(|| {
        VivariumError::Message(format!(
            "need verdict state for target {target} is not an object"
        ))
    })?;
    let Some(value) = needs.get(need_handle).cloned() else {
        return Ok(NeedVerdictState::default());
    };
    serde_json::from_value(value)
        .map_err(|error| VivariumError::Message(format!("invalid need verdict state: {error}")))
}

/// Store one need's verdict state without disturbing other targets or needs.
pub fn set_state(
    st: &mut State,
    target: &str,
    need_handle: &str,
    verdict: &NeedVerdictState,
) -> Result<(), VivariumError> {
    let root = st
        .extra
        .entry(String::from(EXTRA_KEY))
        .or_insert_with(|| serde_json::json!({}));
    let targets = root.as_object_mut().ok_or_else(|| {
        VivariumError::Message(String::from("need verdict state root is not an object"))
    })?;
    let target_state = targets
        .entry(target.to_owned())
        .or_insert_with(|| serde_json::json!({}));
    let needs = target_state.as_object_mut().ok_or_else(|| {
        VivariumError::Message(format!(
            "need verdict state for target {target} is not an object"
        ))
    })?;
    needs.insert(
        need_handle.to_owned(),
        serde_json::to_value(verdict).map_err(|error| {
            VivariumError::Message(format!("encode need verdict state: {error}"))
        })?,
    );
    Ok(())
}

/// Read the accepted receipt only while its full frozen binding is current.
pub fn current_accepted_receipt(
    ctx: &Ctx,
    st: &State,
    need_handle: &str,
    target: &str,
) -> Option<AcceptedNeedReceipt> {
    let verdict_state = match state(st, target, need_handle) {
        Ok(verdict_state) => verdict_state,
        Err(error) => {
            ctx.exception("need-verdict-state", need_handle, &error.to_string());
            return None;
        }
    };
    if verdict_state.hold.is_some() {
        return None;
    }
    let accepted = verdict_state.accepted?;
    if !matches!(
        accepted.receipt.verdict,
        NeedAuditVerdict::CleanPass | NeedAuditVerdict::Residual
    ) || accepted.receipt.job_id != accepted.job_id
    {
        return None;
    }
    let current = match need::binding_for(st, target, need_handle) {
        Ok(Some(binding)) => binding,
        Ok(None) => return None,
        Err(error) => {
            ctx.exception("need-branch-state", need_handle, &error.to_string());
            return None;
        }
    };
    if current != accepted.receipt.binding {
        return None;
    }
    let job = st.jobs.get(&accepted.job_id)?;
    if need_audit::validate_current_job_receipt(ctx, st, job, &accepted.receipt).is_err() {
        return None;
    }
    Some(accepted)
}
