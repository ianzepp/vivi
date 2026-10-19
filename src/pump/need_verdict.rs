//! Durable routing for frozen need-audit receipts.
//!
//! Verdict state lives in `State.extra` so pump.db needs no schema migration.
//! External needs use deterministic subjects and are looked up before send;
//! repair units and verdict state are written together by the pump savepoint.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::VivariumError;
use crate::pump::host::Ctx;
use crate::pump::need::{self, NeedAuditBinding, NeedBranch};
use crate::pump::need_audit::{self, NeedAuditReceipt, NeedAuditVerdict};
use crate::pump::state::{NeedRecord, State, Status, Unit};

mod needs;
use needs::{
    append_bullets, ensure_escalation, ensure_need, finding_need_body, one_line,
    ordinary_recipient, report_task, route_requirement_gaps, short_hash, verdict_label,
};

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

/// Whether a valid audit receipt changed lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteOutcome {
    /// The receipt and its required work were applied.
    Applied,
    /// This exact receipt was already routed.
    Duplicate,
    /// The branch tip or frozen membership changed after this receipt was made.
    Stale,
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
    /// Repair unit by revision and finding ID; a second block on the same finding escalates.
    #[serde(default)]
    pub repair_units: BTreeMap<String, String>,
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

/// Route a validated, completed audit receipt exactly once.
pub fn route_receipt(
    ctx: &Ctx,
    st: &mut State,
    target: &str,
    job_id: &str,
    receipt: &NeedAuditReceipt,
) -> Result<RouteOutcome, VivariumError> {
    let Some(job) = st.jobs.get(job_id) else {
        return Err(VivariumError::Message(format!(
            "need audit job {job_id} is absent from durable pump state"
        )));
    };
    let expected = need_audit::expectation_from_job(job).map_err(VivariumError::Message)?;
    if expected.binding.target != target || expected.binding.need != receipt.binding.need {
        return Err(VivariumError::Message(String::from(
            "need audit receipt target or need differs from its durable job",
        )));
    }
    let prior = state(st, target, &expected.binding.need)?;
    if duplicate_receipt(&prior, receipt)? {
        return Ok(RouteOutcome::Duplicate);
    }
    let Some(current) = need::binding_for(st, target, &expected.binding.need)? else {
        return Ok(RouteOutcome::Stale);
    };
    let Some(branch) = need::get(st, target, &expected.binding.need)? else {
        return Ok(RouteOutcome::Stale);
    };
    if !branch.is_frozen()
        || current != expected.binding
        || branch.audit_base_sha != expected.audit_base
    {
        return Ok(RouteOutcome::Stale);
    }
    need_audit::validate_current_job_receipt(ctx, st, job, receipt)
        .map_err(VivariumError::Message)?;
    route_current_receipt(ctx, st, target, job_id, receipt, &branch, &current)
}

/// Apply a receipt after durable job and current-range identity have been proven.
fn route_current_receipt(
    ctx: &Ctx,
    st: &mut State,
    target: &str,
    job_id: &str,
    receipt: &NeedAuditReceipt,
    branch: &NeedBranch,
    binding: &NeedAuditBinding,
) -> Result<RouteOutcome, VivariumError> {
    let mut verdict_state = state(st, target, &binding.need)?;
    if duplicate_receipt(&verdict_state, receipt)? {
        return Ok(RouteOutcome::Duplicate);
    }
    let mut next = verdict_state.clone();
    let report_reference = receipt.report.reference.clone();
    next.original_binding.get_or_insert_with(|| binding.clone());
    next.ranges.push(NeedAuditRange {
        binding: binding.clone(),
        audit_base: receipt.audit_base.clone(),
        round: receipt.round,
        receipt_key: receipt.receipt_key.clone(),
        report_reference,
    });
    next.receipts
        .insert(receipt.receipt_key.clone(), receipt.clone());
    match receipt.verdict {
        NeedAuditVerdict::CleanPass | NeedAuditVerdict::Residual => {
            route_nonblocking(ctx, &mut next, job_id, receipt)?;
            next.recheck_base = None;
            next.recheck_context = None;
            next.recheck_job_id = None;
            next.accepted = Some(AcceptedNeedReceipt {
                receipt: receipt.clone(),
                job_id: job_id.to_owned(),
            });
        }
        NeedAuditVerdict::BlockRange => {
            next.accepted = None;
            route_block_range(ctx, st, &mut next, target, receipt, branch, binding)?;
        }
        NeedAuditVerdict::BlockShip => {
            next.accepted = None;
            route_block_ship(ctx, &mut next, job_id, receipt)?;
        }
    }
    verdict_state = next;
    set_state(st, target, &binding.need, &verdict_state)?;
    ctx.log(&format!(
        "need audit routed {} verdict={} round={} job={job_id}",
        binding.need,
        verdict_label(receipt.verdict),
        receipt.round
    ));
    Ok(RouteOutcome::Applied)
}

fn duplicate_receipt(
    state: &NeedVerdictState,
    receipt: &NeedAuditReceipt,
) -> Result<bool, VivariumError> {
    let Some(previous) = state.receipts.get(&receipt.receipt_key) else {
        return Ok(false);
    };
    if previous == receipt {
        return Ok(true);
    }
    Err(VivariumError::Message(format!(
        "need audit receipt key {} was reused for different report content",
        receipt.receipt_key
    )))
}

/// File residual findings and every declared requirement gap as ordinary needs.
fn route_nonblocking(
    ctx: &Ctx,
    state: &mut NeedVerdictState,
    job_id: &str,
    receipt: &NeedAuditReceipt,
) -> Result<(), VivariumError> {
    route_requirement_gaps(ctx, state, job_id, receipt)?;
    for finding in &receipt.findings {
        let key = format!("{}:finding:{}", receipt.receipt_key, finding.id);
        let body = finding_need_body(
            job_id,
            receipt,
            &finding.id,
            &finding.content,
            &finding.paths,
            &finding.recheck_trigger,
        );
        let handle = ensure_need(
            ctx,
            &key,
            &format!(
                "pump: audit residual {} {}",
                receipt.binding.need,
                short_hash(&key)
            ),
            &[ordinary_recipient(ctx)],
            &body,
        )?;
        state.residual_needs.insert(key, handle);
    }
    if receipt.verdict == NeedAuditVerdict::Residual
        && receipt.findings.is_empty()
        && !has_requirement_gaps(receipt)
    {
        file_residual_disposition(ctx, state, job_id, receipt)?;
    }
    Ok(())
}

fn has_requirement_gaps(receipt: &NeedAuditReceipt) -> bool {
    receipt.requirements.iter().any(|requirement| {
        !matches!(
            requirement.status,
            crate::pump::need_audit::NeedRequirementStatus::Satisfied
                | crate::pump::need_audit::NeedRequirementStatus::NotApplicable
        )
    })
}

fn file_residual_disposition(
    ctx: &Ctx,
    state: &mut NeedVerdictState,
    job_id: &str,
    receipt: &NeedAuditReceipt,
) -> Result<(), VivariumError> {
    let key = format!("{}:disposition", receipt.receipt_key);
    let mut content = String::from("Residual disposition from the Auditor report.\nDiagnoses:\n");
    append_bullets(&mut content, &receipt.diagnoses);
    content.push_str("Blind spots:\n");
    append_bullets(&mut content, &receipt.blind_spots);
    let trigger = String::from(
        "Review the named audit diagnosis or blind spot and record its explicit disposition.",
    );
    let body = finding_need_body(
        job_id,
        receipt,
        "residual-disposition",
        &content,
        &[],
        &trigger,
    );
    let handle = ensure_need(
        ctx,
        &key,
        &format!(
            "pump: audit residual {} {}",
            receipt.binding.need,
            short_hash(&key)
        ),
        &[ordinary_recipient(ctx)],
        &body,
    )?;
    state.residual_needs.insert(key, handle);
    Ok(())
}

/// Create repair units on the same need branch and schedule only the bounded repair review.
fn route_block_range(
    ctx: &Ctx,
    st: &mut State,
    state: &mut NeedVerdictState,
    target: &str,
    receipt: &NeedAuditReceipt,
    branch: &NeedBranch,
    binding: &NeedAuditBinding,
) -> Result<(), VivariumError> {
    route_requirement_gaps(ctx, state, &receipt.job_id, receipt)?;
    let mut created = Vec::new();
    let mut exhausted = Vec::new();
    for finding in &receipt.findings {
        let finding_key = repair_finding_key(binding.revision, &finding.id);
        if let Some(unit) = state.repair_units.get(&finding_key) {
            exhausted.push((finding.id.clone(), unit.clone(), finding.paths.clone()));
            continue;
        }
        let uid = repair_unit_id(binding, &receipt.receipt_key, &finding.id);
        state.repair_units.insert(finding_key, uid.clone());
        created.push((finding, uid));
    }
    if !created.is_empty() {
        need::begin_repair(st, target, &binding.need, binding.revision)?;
        for (finding, uid) in created {
            insert_repair_unit(ctx, st, target, &uid, receipt, finding)?;
        }
    }
    if !exhausted.is_empty() {
        hold_repeated_block(ctx, state, receipt, &exhausted[0].0)?;
        state.recheck_base = None;
        state.recheck_context = None;
        state.recheck_job_id = None;
    } else {
        state.recheck_base = Some(binding.tip.clone());
        state.recheck_round = receipt.round.saturating_add(1);
        state.recheck_context = Some(repair_context(state, receipt, branch));
        state.recheck_job_id = None;
    }
    Ok(())
}

/// Escalate a finding that remains blocked after its one repair re-review.
fn hold_repeated_block(
    ctx: &Ctx,
    state: &mut NeedVerdictState,
    receipt: &NeedAuditReceipt,
    finding_id: &str,
) -> Result<(), VivariumError> {
    let reason =
        format!("the same blocked finding remains after its two charter rounds: {finding_id}");
    let mut boundary: Vec<String> = receipt
        .findings
        .iter()
        .flat_map(|finding| finding.paths.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if boundary.is_empty() {
        boundary.push(String::from("declared blocked boundary"));
    }
    let escalation = ensure_escalation(ctx, receipt, &reason, &boundary)?;
    state.hold = Some(NeedAuditHold {
        reason,
        receipt_key: receipt.receipt_key.clone(),
        escalation_need: escalation,
        boundary,
    });
    Ok(())
}

/// Mint one internal repair unit with deterministic identity and bounded write scope.
fn insert_repair_unit(
    ctx: &Ctx,
    st: &mut State,
    target: &str,
    uid: &str,
    receipt: &NeedAuditReceipt,
    finding: &crate::pump::need_audit::NeedAuditFinding,
) -> Result<(), VivariumError> {
    if st.units.contains_key(uid) {
        return Ok(());
    }
    let report_task = report_task(receipt);
    let mut brief = format!(
        "Repair the blocking finding from need audit {}.\nNeed: {}\nFull frozen range: {}..{}\nReview diff: {}..{}\nFinding {}: {}\nAffected paths:\n",
        report_task,
        receipt.binding.need,
        receipt.binding.base,
        receipt.binding.tip,
        receipt.audit_base,
        receipt.binding.tip,
        finding.id,
        one_line(&finding.content),
    );
    append_bullets(&mut brief, &finding.paths);
    brief.push_str(&format!(
        "\nAudit report: {}\nRecheck trigger: {}\n\ndone_when: {}\n",
        one_line(&receipt.report.reference),
        one_line(&finding.recheck_trigger),
        one_line(&finding.recheck_trigger),
    ));
    let unit = Unit {
        need: receipt.binding.need.clone(),
        target: target.to_owned(),
        revision: receipt.binding.revision,
        title: format!("Repair audited finding {}", finding.id),
        brief,
        write_scope: finding.paths.clone(),
        status: Status::Pending,
        since: ctx.now(),
        ..Unit::default()
    };
    st.units.insert(uid.to_owned(), unit);
    st.order.push(uid.to_owned());
    let need_record = st
        .needs
        .entry(receipt.binding.need.clone())
        .or_insert_with(|| NeedRecord {
            verdict: String::from("planned"),
            reason: String::from("audit block-range repair units"),
            revision: receipt.binding.revision,
            ..NeedRecord::default()
        });
    if !need_record.units.iter().any(|existing| existing == uid) {
        need_record.units.push(uid.to_owned());
    }
    Ok(())
}

/// Hold the branch and send one escalation need to both operator and head-cto.
fn route_block_ship(
    ctx: &Ctx,
    state: &mut NeedVerdictState,
    job_id: &str,
    receipt: &NeedAuditReceipt,
) -> Result<(), VivariumError> {
    route_requirement_gaps(ctx, state, job_id, receipt)?;
    let reason = receipt
        .diagnoses
        .first()
        .cloned()
        .unwrap_or_else(|| String::from("auditor returned block_ship"));
    let boundary = receipt
        .findings
        .iter()
        .flat_map(|finding| finding.paths.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let escalation = ensure_escalation(ctx, receipt, &reason, &boundary)?;
    state.hold = Some(NeedAuditHold {
        reason: format!("{reason} (audit job {job_id})"),
        receipt_key: receipt.receipt_key.clone(),
        escalation_need: escalation,
        boundary,
    });
    state.recheck_base = None;
    state.recheck_context = None;
    state.recheck_job_id = None;
    Ok(())
}

/// Return a repair review whose units have settled and whose branch is frozen.
pub fn pending_reaudit(
    st: &State,
    target: &str,
    need_handle: &str,
) -> Result<Option<NeedRepairAuditRequest>, VivariumError> {
    let verdict_state = state(st, target, need_handle)?;
    let Some(audit_base) = verdict_state.recheck_base else {
        return Ok(None);
    };
    if verdict_state.recheck_job_id.is_some() {
        return Ok(None);
    }
    let Some(binding) = need::binding_for(st, target, need_handle)? else {
        return Ok(None);
    };
    let Some(branch) = need::get(st, target, need_handle)? else {
        return Ok(None);
    };
    if !branch.is_frozen() || branch.audit_base_sha != audit_base {
        return Ok(None);
    }
    Ok(Some(NeedRepairAuditRequest {
        binding,
        audit_base,
        round: verdict_state.recheck_round,
        context: verdict_state.recheck_context.unwrap_or_default(),
    }))
}

/// Enqueue ready repair-only audits through the same durable auditor role path.
pub fn dispatch_pending_reaudits(ctx: &Ctx, st: &mut State) -> Result<Vec<String>, VivariumError> {
    let pending = pending_keys(st)?;
    let mut queued = Vec::new();
    for (target, need_handle) in pending {
        let Some(request) = pending_reaudit(st, &target, &need_handle)? else {
            continue;
        };
        if request.binding.tip == request.audit_base {
            escalate_empty_repair(ctx, st, &target, &need_handle)?;
            continue;
        }
        let job_id = need_audit::dispatch_frozen(
            ctx,
            st,
            request.binding.clone(),
            &request.audit_base,
            request.round,
            &request.context,
        )?;
        let mut verdict_state = state(st, &target, &need_handle)?;
        verdict_state.recheck_job_id = Some(job_id.clone());
        set_state(st, &target, &need_handle, &verdict_state)?;
        queued.push(job_id);
    }
    Ok(queued)
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

/// Compact repair provenance included in the next read-only audit brief.
fn repair_context(
    state: &NeedVerdictState,
    receipt: &NeedAuditReceipt,
    branch: &NeedBranch,
) -> String {
    let mut context = format!(
        "Re-review the repair diff only. Original full range: {}..{}. Prior frozen tip: {}. New review range must start at that prior frozen tip.\n",
        receipt.binding.base, receipt.binding.tip, receipt.binding.tip,
    );
    if !branch.audit_base_sha.is_empty() {
        context.push_str(&format!(
            "Previous audit started at {}; retain that earlier range in the provenance ledger.\n",
            branch.audit_base_sha
        ));
    }
    let mut receipts: Vec<&NeedAuditReceipt> = state.receipts.values().collect();
    receipts.sort_by_key(|prior| (prior.round, prior.receipt_key.as_str()));
    for prior in receipts {
        append_prior_receipt(&mut context, prior);
    }
    context
}

fn append_prior_receipt(out: &mut String, receipt: &NeedAuditReceipt) {
    out.push_str(&format!(
        "Prior receipt {} round {}: verdict={}, full_range={}..{}, review_range={}..{}, report={}.\n",
        receipt.receipt_key,
        receipt.round,
        verdict_label(receipt.verdict),
        receipt.binding.base,
        receipt.binding.tip,
        receipt.audit_base,
        receipt.binding.tip,
        receipt.report.reference,
    ));
    append_labeled(out, "Diagnosis", &receipt.diagnoses);
    append_labeled(out, "Blind spot", &receipt.blind_spots);
    for finding in &receipt.findings {
        out.push_str(&format!(
            "Finding {}: {}; paths={}; recheck={}.\n",
            finding.id,
            finding.content,
            finding.paths.join(", "),
            finding.recheck_trigger,
        ));
    }
}

fn append_labeled(out: &mut String, label: &str, values: &[String]) {
    for value in values {
        out.push_str(&format!("{label}: {value}\n"));
    }
}

/// Surface a repair that produced no new tip instead of silently leaving it blocked.
fn escalate_empty_repair(
    ctx: &Ctx,
    st: &mut State,
    target: &str,
    need_handle: &str,
) -> Result<(), VivariumError> {
    let mut verdict_state = state(st, target, need_handle)?;
    let Some(receipt) = verdict_state
        .receipts
        .values()
        .filter(|receipt| receipt.verdict == NeedAuditVerdict::BlockRange)
        .max_by_key(|receipt| receipt.round)
        .cloned()
    else {
        return Ok(());
    };
    let reason = String::from("repair units settled without changing the frozen branch tip");
    let boundary: Vec<String> = receipt
        .findings
        .iter()
        .flat_map(|finding| finding.paths.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let escalation = ensure_escalation(ctx, &receipt, &reason, &boundary)?;
    verdict_state.hold = Some(NeedAuditHold {
        reason,
        receipt_key: receipt.receipt_key.clone(),
        escalation_need: escalation,
        boundary,
    });
    verdict_state.recheck_base = None;
    verdict_state.recheck_context = None;
    verdict_state.recheck_job_id = None;
    set_state(st, target, need_handle, &verdict_state)
}

/// Enumerate state entries that may have a queued repair audit.
fn pending_keys(st: &State) -> Result<Vec<(String, String)>, VivariumError> {
    let Some(root) = st.extra.get(EXTRA_KEY) else {
        return Ok(Vec::new());
    };
    let targets = root.as_object().ok_or_else(|| {
        VivariumError::Message(String::from("need verdict state root is not an object"))
    })?;
    let mut keys = Vec::new();
    for (target, target_state) in targets {
        let needs = target_state.as_object().ok_or_else(|| {
            VivariumError::Message(format!(
                "need verdict state for target {target} is not an object"
            ))
        })?;
        for need_handle in needs.keys() {
            keys.push((target.clone(), need_handle.clone()));
        }
    }
    Ok(keys)
}

/// Stable internal repair unit identity for one audited finding.
fn repair_unit_id(binding: &NeedAuditBinding, receipt_key: &str, finding_id: &str) -> String {
    let value = format!(
        "{}:{}:{}:{}:{}",
        binding.target, binding.need, binding.revision, receipt_key, finding_id
    );
    format!("na-{}", short_hash(&value))
}

fn repair_finding_key(revision: u64, finding_id: &str) -> String {
    format!("r{revision}:{finding_id}")
}
