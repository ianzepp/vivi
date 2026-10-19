//! Durable landing of accepted, frozen need branches onto a target's
//! integration branch. Git writes stay in Merge-role pool jobs; this module
//! only queues those jobs and validates their durable runner receipts.

use serde::{Deserialize, Serialize};

use crate::error::VivariumError;
use crate::pump::need_verdict::AcceptedNeedReceipt;

mod collect;
mod queue;
pub(crate) use collect::collect_completed;
pub(crate) use queue::dispatch;

const MERGE_RESULT: &str = "PUMP_MERGE_RESULT=";

#[derive(Debug, Clone, Deserialize)]
pub(super) struct NeedMergeResult {
    version: u32,
    target: String,
    #[serde(default)]
    need: Option<String>,
    #[serde(default)]
    revision: Option<u64>,
    #[serde(default)]
    base: Option<String>,
    tip: String,
    candidate: String,
    status: String,
    #[serde(default)]
    checks: Vec<NeedMergeCheck>,
    sources: Vec<NeedMergeSource>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct NeedMergeCheck {
    name: String,
    command: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct NeedMergeSource {
    kind: String,
    id: String,
    status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RequiredCheck {
    name: String,
    command: String,
}

fn verdict_name(accepted: &AcceptedNeedReceipt) -> Result<String, VivariumError> {
    match accepted.receipt.verdict {
        crate::pump::need_audit::NeedAuditVerdict::CleanPass => Ok(String::from("clean_pass")),
        crate::pump::need_audit::NeedAuditVerdict::Residual => Ok(String::from("residual")),
        _ => Err(VivariumError::Message(String::from(
            "accepted need verdict is invalid",
        ))),
    }
}

pub(super) fn full_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "need_landing_test.rs"]
mod tests;
