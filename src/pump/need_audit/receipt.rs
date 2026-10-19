//! Structured, identity-bound need audit reports.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::pump::jobs::{self, PoolJob};
use crate::pump::need::NeedAuditBinding;

use super::{NeedAuditRequirement, validate_binding, validate_requirement_specs};

pub(super) const RESULT_PREFIX: &str = "PUMP_NEED_AUDIT_RESULT=";

/// A verdict supported by the standing Auditor charter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedAuditVerdict {
    CleanPass,
    Residual,
    BlockRange,
    BlockShip,
}

/// Status of one original need requirement in an Auditor report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NeedRequirementStatus {
    Satisfied,
    Partial,
    Missing,
    Unverified,
    NotApplicable,
}

/// Audited status and supporting evidence for one original requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeedAuditRequirementResult {
    pub id: String,
    pub text: String,
    pub status: NeedRequirementStatus,
    pub evidence: Vec<String>,
}

/// One durable finding that the verdict router can turn into follow-up work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeedAuditFinding {
    pub id: String,
    pub content: String,
    pub paths: Vec<String>,
    pub recheck_trigger: String,
}

/// Auditor-reported link to the durable report and its controller source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeedAuditReportProvenance {
    pub id: String,
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collected_at: Option<u64>,
}

/// Structured report retained with its durable need/range/job identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeedAuditReceipt {
    pub version: u32,
    pub receipt_key: String,
    pub job_id: String,
    pub binding: NeedAuditBinding,
    pub source_hash: String,
    /// Review range start; this can be a prior frozen tip during repair review.
    pub audit_base: String,
    /// One-based report round for this need/revision.
    pub round: u32,
    pub verdict: NeedAuditVerdict,
    pub requirements: Vec<NeedAuditRequirementResult>,
    #[serde(default)]
    pub findings: Vec<NeedAuditFinding>,
    #[serde(default)]
    pub diagnoses: Vec<String>,
    #[serde(default)]
    pub blind_spots: Vec<String>,
    pub report: NeedAuditReportProvenance,
}

/// The exact identity and original requirements expected from one job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeedAuditExpectation {
    pub binding: NeedAuditBinding,
    pub source_hash: String,
    pub job_id: String,
    pub audit_base: String,
    pub round: u32,
    pub requirements: Vec<NeedAuditRequirement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JobPayload {
    pub(super) binding: NeedAuditBinding,
    pub(super) source_hash: String,
    pub(super) audit_base: String,
    pub(super) round: u32,
    pub(super) requirements: Vec<NeedAuditRequirement>,
}

/// Reconstruct the validation expectation from the durable job after restart.
pub fn expectation_from_job(job: &PoolJob) -> Result<NeedAuditExpectation, String> {
    if job.kind != "need-audit" {
        return Err(format!("job {} is not a need acceptance audit", job.id));
    }
    let payload: JobPayload = serde_json::from_value(job.payload.clone())
        .map_err(|error| format!("need audit job {} has invalid payload: {error}", job.id))?;
    let expected_id = need_audit_job_id(
        &payload.binding,
        &payload.source_hash,
        &payload.audit_base,
        payload.round,
    );
    if job.id != expected_id {
        return Err(format!(
            "need audit job {} has a stale or incorrect identity",
            job.id
        ));
    }
    let expected = NeedAuditExpectation {
        binding: payload.binding,
        source_hash: payload.source_hash,
        job_id: job.id.clone(),
        audit_base: payload.audit_base,
        round: payload.round,
        requirements: payload.requirements,
    };
    validate_expectation(&expected)?;
    Ok(expected)
}

/// Parse and validate the final marked Auditor response for an expected job.
pub fn parse_receipt(
    raw: &str,
    expected: &NeedAuditExpectation,
) -> Result<NeedAuditReceipt, String> {
    let message = final_message(raw);
    let json = message
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(RESULT_PREFIX))
        .ok_or_else(|| String::from("need audit response omitted its result marker"))?;
    let mut receipt: NeedAuditReceipt = serde_json::from_str(json.trim())
        .map_err(|error| format!("need audit response is malformed: {error}"))?;
    // Provenance fields are controller-owned even though the same shape is
    // used for serialization after collection.
    receipt.report.task = None;
    receipt.report.pool = None;
    receipt.report.collected_at = None;
    validate_receipt(&receipt, expected)?;
    Ok(receipt)
}

/// Parse a terminal durable job and attach the source task/pool receipt data.
pub fn parse_job_receipt(job: &PoolJob) -> Result<NeedAuditReceipt, String> {
    let expected = expectation_from_job(job)?;
    let outcome = job
        .outcome
        .as_ref()
        .ok_or_else(|| format!("need audit job {} has no collected outcome", job.id))?;
    if outcome.state != "finished" || job.status != jobs::JobStatus::Finished {
        return Err(format!("need audit job {} did not finish", job.id));
    }
    if outcome.agent_success() != Some(true) {
        return Err(format!(
            "need audit job {} has no successful worker exit",
            job.id
        ));
    }
    if job.task.as_deref().is_none_or(str::is_empty)
        || job.pool.as_deref().is_none_or(str::is_empty)
    {
        return Err(format!(
            "need audit job {} is missing task or pool provenance",
            job.id
        ));
    }
    let mut receipt = parse_receipt(&outcome.receipt, &expected)?;
    receipt.report.task.clone_from(&job.task);
    receipt.report.pool.clone_from(&job.pool);
    receipt.report.collected_at = Some(outcome.at);
    Ok(receipt)
}

/// Reject malformed, stale, incomplete, or contradictory audit reports.
pub fn validate_receipt(
    receipt: &NeedAuditReceipt,
    expected: &NeedAuditExpectation,
) -> Result<(), String> {
    validate_expectation(expected)?;
    validate_receipt_identity(receipt, expected)?;
    validate_requirement_results(&receipt.requirements, &expected.requirements)?;
    validate_findings(&receipt.findings)?;
    validate_verdict_shape(receipt)?;
    validate_report_notes(receipt)?;
    Ok(())
}

/// Stable job identity for one frozen binding and review round.
#[must_use]
pub fn need_audit_job_id(
    binding: &NeedAuditBinding,
    source_hash: &str,
    audit_base: &str,
    round: u32,
) -> String {
    format!(
        "need-audit-job-{}",
        digest_key(binding, source_hash, audit_base, round)
    )
}

/// Stable report key derived from the durable job identity.
#[must_use]
pub fn receipt_key(expected: &NeedAuditExpectation) -> String {
    let key = Sha256::digest(expected.job_id.as_bytes());
    format!("need-audit-report-{}", hex::encode(key))
}

fn digest_key(
    binding: &NeedAuditBinding,
    source_hash: &str,
    audit_base: &str,
    round: u32,
) -> String {
    let mut hasher = Sha256::new();
    let revision = binding.revision.to_string();
    let round = round.to_string();
    for field in [
        binding.need.as_str(),
        binding.target.as_str(),
        revision.as_str(),
        binding.base.as_str(),
        binding.tip.as_str(),
        source_hash,
        audit_base,
        round.as_str(),
    ] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn validate_expectation(expected: &NeedAuditExpectation) -> Result<(), String> {
    validate_binding(&expected.binding)?;
    if !super::is_full_commit_sha(&expected.audit_base) || expected.round == 0 {
        return Err(String::from(
            "need audit expectation has an invalid review range or round",
        ));
    }
    if !is_source_hash(&expected.source_hash)
        || expected.job_id
            != need_audit_job_id(
                &expected.binding,
                &expected.source_hash,
                &expected.audit_base,
                expected.round,
            )
    {
        return Err(String::from(
            "need audit expectation has an incorrect job identity",
        ));
    }
    validate_requirement_specs(&expected.requirements)
}

fn is_source_hash(value: &str) -> bool {
    value.strip_prefix("v3:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn validate_receipt_identity(
    receipt: &NeedAuditReceipt,
    expected: &NeedAuditExpectation,
) -> Result<(), String> {
    if receipt.version != 1 {
        return Err(format!(
            "unsupported need audit receipt version {}",
            receipt.version
        ));
    }
    if receipt.binding != expected.binding {
        return Err(String::from(
            "need audit receipt does not match the frozen need binding",
        ));
    }
    if receipt.source_hash != expected.source_hash {
        return Err(String::from(
            "need audit receipt names a different source-thread snapshot",
        ));
    }
    if receipt.job_id != expected.job_id {
        return Err(String::from(
            "need audit receipt names a different durable job",
        ));
    }
    if receipt.audit_base != expected.audit_base || receipt.round != expected.round {
        return Err(String::from(
            "need audit receipt names a stale review range or round",
        ));
    }
    if receipt.receipt_key != receipt_key(expected) {
        return Err(String::from(
            "need audit receipt key does not match its dispatch identity",
        ));
    }
    if receipt.report.id.trim().is_empty() || receipt.report.reference.trim().is_empty() {
        return Err(String::from("need audit report provenance is incomplete"));
    }
    Ok(())
}

fn validate_verdict_shape(receipt: &NeedAuditReceipt) -> Result<(), String> {
    match receipt.verdict {
        NeedAuditVerdict::CleanPass => {
            if !receipt.findings.is_empty()
                || receipt.requirements.iter().any(|requirement| {
                    !matches!(
                        requirement.status,
                        NeedRequirementStatus::Satisfied | NeedRequirementStatus::NotApplicable
                    )
                })
            {
                return Err(String::from(
                    "clean_pass conflicts with findings or unmet requirement statuses",
                ));
            }
        }
        NeedAuditVerdict::BlockRange | NeedAuditVerdict::BlockShip => {
            if receipt.findings.is_empty() {
                return Err(String::from("blocking audit verdict has no finding"));
            }
        }
        NeedAuditVerdict::Residual => {
            let has_unmet_requirement = receipt.requirements.iter().any(|requirement| {
                !matches!(
                    requirement.status,
                    NeedRequirementStatus::Satisfied | NeedRequirementStatus::NotApplicable
                )
            });
            if receipt.findings.is_empty()
                && receipt.blind_spots.is_empty()
                && receipt.diagnoses.is_empty()
                && !has_unmet_requirement
            {
                return Err(String::from(
                    "residual audit verdict has no finding or evidence gap",
                ));
            }
        }
    }
    Ok(())
}

fn validate_report_notes(receipt: &NeedAuditReceipt) -> Result<(), String> {
    if receipt
        .diagnoses
        .iter()
        .chain(&receipt.blind_spots)
        .any(|entry| entry.trim().is_empty())
    {
        return Err(String::from(
            "need audit contains an empty diagnosis or blind spot",
        ));
    }
    Ok(())
}

fn validate_requirement_results(
    results: &[NeedAuditRequirementResult],
    expected: &[NeedAuditRequirement],
) -> Result<(), String> {
    if results.len() != expected.len() {
        return Err(String::from(
            "need audit omitted or added original requirements",
        ));
    }
    let mut actual = std::collections::BTreeMap::new();
    for result in results {
        if result.id.trim().is_empty() || result.text.trim().is_empty() {
            return Err(String::from(
                "need audit has an incomplete requirement result",
            ));
        }
        if result.evidence.is_empty() || result.evidence.iter().any(|item| item.trim().is_empty()) {
            return Err(format!(
                "requirement {} has no usable evidence or reason",
                result.id
            ));
        }
        if actual.insert(result.id.as_str(), result).is_some() {
            return Err(format!("need audit repeats requirement {}", result.id));
        }
    }
    for requirement in expected {
        let Some(result) = actual.get(requirement.id.as_str()) else {
            return Err(format!("need audit omitted requirement {}", requirement.id));
        };
        if result.text != requirement.text {
            return Err(format!(
                "need audit changed the text of requirement {}",
                requirement.id
            ));
        }
    }
    Ok(())
}

fn validate_findings(findings: &[NeedAuditFinding]) -> Result<(), String> {
    let mut ids = std::collections::BTreeSet::new();
    for finding in findings {
        if finding.id.trim().is_empty()
            || finding.content.trim().is_empty()
            || finding.paths.is_empty()
            || finding.paths.iter().any(|path| path.trim().is_empty())
            || finding.recheck_trigger.trim().is_empty()
        {
            return Err(String::from(
                "need audit finding is missing content, path, or recheck trigger",
            ));
        }
        if !ids.insert(finding.id.as_str()) {
            return Err(format!("need audit repeats finding {}", finding.id));
        }
    }
    Ok(())
}

fn final_message(raw: &str) -> &str {
    raw.rsplit_once("hand's final message:")
        .map_or(raw, |(_, message)| message)
}
