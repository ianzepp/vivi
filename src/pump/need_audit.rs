//! Durable acceptance audits for frozen need branches.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::VivariumError;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::jobs::{self, JobSpec, PoolJob};
use crate::pump::state::State;
use crate::routing::RoutedRole;

pub use crate::pump::need::NeedAuditBinding;
use receipt::{JobPayload, RESULT_PREFIX};
pub use receipt::{
    NeedAuditExpectation, NeedAuditFinding, NeedAuditReceipt, NeedAuditReportProvenance,
    NeedAuditRequirementResult, NeedAuditVerdict, NeedRequirementStatus, expectation_from_job,
    need_audit_job_id, parse_job_receipt, parse_receipt, receipt_key, validate_receipt,
};

mod receipt;

/// One original requirement passed to the Auditor for status accounting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeedAuditRequirement {
    pub id: String,
    pub text: String,
}

/// One settled unit included in a frozen need audit packet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeedAuditUnit {
    pub id: String,
    pub title: String,
    pub brief: String,
    pub status: String,
    pub write_scope: Vec<String>,
    pub proof: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip: Option<String>,
}

/// Context needed to audit the frozen range against its original need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeedAuditContext {
    pub original_need_thread: String,
    pub current_plan: String,
    pub source_hash: String,
    pub requirements: Vec<NeedAuditRequirement>,
    pub units: Vec<NeedAuditUnit>,
    pub changed_paths: Vec<String>,
    pub review_context: String,
}

/// Queue an idempotent ordinary auditor-role job for one frozen need range.
pub fn dispatch_frozen(
    ctx: &Ctx,
    st: &mut State,
    binding: NeedAuditBinding,
    audit_base: &str,
    round: u32,
    review_context: &str,
) -> Result<String, VivariumError> {
    let context = context_for(ctx, st, &binding, audit_base, review_context)?;
    validate_context(&binding, audit_base, round, &context)?;
    let job_id = need_audit_job_id(&binding, &context.source_hash, audit_base, round);
    if existing_dispatch(st, &job_id, &binding, audit_base, round, &context)? {
        return Ok(job_id);
    }
    let target_ctx = ctx.for_target(&binding.target).ok_or_else(|| {
        VivariumError::Config(format!(
            "cannot dispatch need audit for unknown target {:?}",
            binding.target
        ))
    })?;
    let (role, pool) = auditor_route(ctx)?;
    let body = audit_brief(&job_id, &binding, audit_base, round, &context);
    let mut spec = JobSpec::new(
        job_id.clone(),
        "need-audit",
        role.name.clone(),
        format!(
            "acceptance audit {} revision {} round {round}",
            binding.need, binding.revision
        ),
        body,
    );
    spec.pool_candidates = ctx.routing.fallback_chain(pool);
    spec.target = Some(binding.target.clone());
    spec.from_ref = Some(binding.tip.clone());
    spec.with = (audit_base != binding.tip)
        .then(|| audit_base.to_owned())
        .into_iter()
        .collect();
    spec.budget = target_ctx.cfg.dispatch.budget;
    spec.read_only = true;
    spec.payload = serde_json::to_value(JobPayload {
        binding,
        source_hash: context.source_hash,
        audit_base: audit_base.to_owned(),
        round,
        requirements: context.requirements,
    })
    .map_err(|error| VivariumError::Message(format!("encode need audit job: {error}")))?;
    jobs::enqueue(st, spec)?;
    ctx.log(&format!("need acceptance audit queued {job_id}"));
    Ok(job_id)
}

fn existing_dispatch(
    st: &State,
    job_id: &str,
    binding: &NeedAuditBinding,
    audit_base: &str,
    round: u32,
    context: &NeedAuditContext,
) -> Result<bool, VivariumError> {
    if let Some(existing) = st.jobs.get(job_id) {
        let expected = expectation_from_job(existing).map_err(|error| {
            VivariumError::Config(format!(
                "existing need audit job {job_id} is invalid: {error}"
            ))
        })?;
        if &expected.binding == binding
            && expected.source_hash == context.source_hash
            && expected.audit_base == audit_base
            && expected.round == round
            && expected.requirements == context.requirements
        {
            return Ok(true);
        }
        return Err(VivariumError::Config(format!(
            "existing need audit job {job_id} does not match its dispatch identity"
        )));
    }
    Ok(false)
}

/// Build a fresh packet from the need thread, current plan and frozen branch.
pub fn context_for(
    ctx: &Ctx,
    st: &State,
    binding: &NeedAuditBinding,
    audit_base: &str,
    review_context: &str,
) -> Result<NeedAuditContext, VivariumError> {
    validate_binding(binding).map_err(VivariumError::Config)?;
    ensure_current_binding(st, binding)?;
    validate_review_base(st, binding, audit_base)?;
    let record = st.needs.get(&binding.need).ok_or_else(|| {
        VivariumError::Config(format!("need {} has no current plan record", binding.need))
    })?;
    if record.amendment_pending || record.revision != binding.revision {
        return Err(VivariumError::Config(format!(
            "need {} has an unprocessed amendment or stale plan revision",
            binding.need
        )));
    }
    let target_ctx = ctx.for_target(&binding.target).ok_or_else(|| {
        VivariumError::Config(format!(
            "cannot read need audit target {:?}",
            binding.target
        ))
    })?;
    ensure_audit_range_objects(&target_ctx, st, binding, audit_base)?;
    let messages = need_thread(&target_ctx, &binding.need)?;
    let source_hash = validate_thread_revision(
        &target_ctx,
        &binding.need,
        record.thread_sha256.as_deref(),
        &messages,
    )?;
    let requirements = thread_requirements(&target_ctx, &binding.need, &messages)?;
    let units = need_units(st, binding)?;
    let changed_paths = changed_paths(&target_ctx, audit_base, &binding.tip)?;
    let current_plan = current_plan(binding, record, &units)?;
    let source_messages = source_messages(&target_ctx, &binding.need, &messages);
    let original_need_thread = serde_json::to_string_pretty(&source_messages).map_err(|error| {
        VivariumError::Message(format!("encode original need thread for audit: {error}"))
    })?;
    Ok(NeedAuditContext {
        original_need_thread,
        current_plan,
        source_hash,
        requirements,
        units,
        changed_paths,
        review_context: review_context.to_owned(),
    })
}

/// Validate a report against both its durable job and the current frozen
/// branch record, including the current plan/unit fingerprint.
pub fn validate_current_receipt(
    ctx: &Ctx,
    st: &State,
    receipt: &NeedAuditReceipt,
    expected: &NeedAuditExpectation,
) -> Result<(), String> {
    let current = crate::pump::need::binding_for(
        st,
        expected.binding.target.as_str(),
        expected.binding.need.as_str(),
    )
    .map_err(|error| format!("cannot validate current need binding: {error}"))?;
    if current.as_ref() != Some(&expected.binding) {
        return Err(String::from(
            "need audit receipt is stale for the current frozen branch",
        ));
    }
    let record = st
        .needs
        .get(&expected.binding.need)
        .ok_or_else(|| String::from("need audit receipt has no current plan record"))?;
    if record.amendment_pending || record.revision != expected.binding.revision {
        return Err(String::from(
            "need audit receipt is stale for the current plan revision",
        ));
    }
    if receipt.report.task.as_deref().is_none_or(str::is_empty)
        || receipt.report.pool.as_deref().is_none_or(str::is_empty)
        || receipt.report.collected_at.is_none()
    {
        return Err(String::from(
            "need audit receipt is missing controller dispatch provenance",
        ));
    }
    let target_ctx = ctx
        .for_target(&expected.binding.target)
        .ok_or_else(|| String::from("need audit receipt names an unknown target"))?;
    validate_review_base(st, &expected.binding, &expected.audit_base)
        .map_err(|error| error.to_string())?;
    ensure_audit_range_objects(&target_ctx, st, &expected.binding, &expected.audit_base)
        .map_err(|error| error.to_string())?;
    let messages =
        need_thread(&target_ctx, &expected.binding.need).map_err(|error| error.to_string())?;
    let current_source_hash = validate_thread_revision(
        &target_ctx,
        &expected.binding.need,
        record.thread_sha256.as_deref(),
        &messages,
    )
    .map_err(|error| error.to_string())?;
    if current_source_hash != expected.source_hash {
        return Err(String::from(
            "need audit receipt is stale for the current source-thread snapshot",
        ));
    }
    validate_receipt(receipt, expected)
}

/// Validate a receipt against its source job and the current frozen need.
pub fn validate_current_job_receipt(
    ctx: &Ctx,
    st: &State,
    job: &PoolJob,
    receipt: &NeedAuditReceipt,
) -> Result<(), String> {
    let collected = parse_job_receipt(job)?;
    if &collected != receipt {
        return Err(String::from(
            "need audit report differs from the durable job receipt",
        ));
    }
    let expected = expectation_from_job(job)?;
    validate_current_receipt(ctx, st, receipt, &expected)
}

fn auditor_route(ctx: &Ctx) -> Result<(&RoutedRole, &str), VivariumError> {
    let role = ctx
        .routing
        .auditor()
        .filter(|role| role.status == "active")
        .ok_or_else(|| {
            VivariumError::Config("need acceptance audits require an active auditor role".into())
        })?;
    let pool = ctx.routing.pool_of(role).ok_or_else(|| {
        VivariumError::Config(format!(
            "auditor role {} must name a configured pool for durable acceptance audits",
            role.name
        ))
    })?;
    Ok((role, pool))
}

fn audit_brief(
    job_id: &str,
    binding: &NeedAuditBinding,
    audit_base: &str,
    round: u32,
    context: &NeedAuditContext,
) -> String {
    let expected = NeedAuditExpectation {
        binding: binding.clone(),
        source_hash: context.source_hash.clone(),
        job_id: job_id.to_owned(),
        audit_base: audit_base.to_owned(),
        round,
        requirements: context.requirements.clone(),
    };
    let receipt_key = receipt_key(&expected);
    let packet = serde_json::json!({
        "original_need_thread": context.original_need_thread,
        "current_plan": context.current_plan,
        "source_hash": context.source_hash,
        "requirements": context.requirements,
        "units": context.units,
        "changed_paths": context.changed_paths,
        "review_context": context.review_context,
    });
    format!(
        "You are the ordinary Vivi Auditor role. Follow your standing auditor charter, including its verification policy and width. This is a read-only acceptance audit; do not implement changes, merge, or issue a GO stamp. Do not replace the charter with a pump-specific suite or corpus.\n\nDispatch identity: job_id={job_id}; receipt_key={receipt_key}; need={}; target={}; revision={}; frozen_binding={}..{}; source_hash={}; review_range={audit_base}..{}. Round: {round}. The frozen binding always covers the original need branch base through its current tip. For a repair-only re-review, review the narrower `review_range` while retaining the full binding above.\n\nAccount for each original requirement exactly once and preserve its text. Use only SATISFIED, PARTIAL, MISSING, UNVERIFIED, or NOT_APPLICABLE. Include concrete evidence or a reason for every status. Preserve diagnoses and explicit blind spots. For every finding include a stable id, content, affected paths, and a recheck trigger. Link the report you are returning.\n\nAudit packet JSON:\n{packet}\n\nEnd your final response with one minified JSON line prefixed exactly `{RESULT_PREFIX}`. Required shape:\n{{\"version\":1,\"receipt_key\":\"{receipt_key}\",\"job_id\":\"{job_id}\",\"binding\":{{\"need\":\"{}\",\"target\":\"{}\",\"revision\":{},\"base\":\"{}\",\"tip\":\"{}\"}},\"source_hash\":\"{}\",\"audit_base\":\"{audit_base}\",\"round\":{round},\"verdict\":\"clean_pass|residual|block_range|block_ship\",\"requirements\":[{{\"id\":\"<id>\",\"text\":\"<original text>\",\"status\":\"SATISFIED|PARTIAL|MISSING|UNVERIFIED|NOT_APPLICABLE\",\"evidence\":[\"<evidence or reason>\"]}}],\"findings\":[{{\"id\":\"<finding id>\",\"content\":\"<finding>\",\"paths\":[\"<path>\"],\"recheck_trigger\":\"<observable trigger>\"}}],\"diagnoses\":[\"...\"],\"blind_spots\":[\"...\"],\"report\":{{\"id\":\"<report id>\",\"reference\":\"<report link or durable location>\"}}}}",
        binding.need,
        binding.target,
        binding.revision,
        binding.base,
        binding.tip,
        context.source_hash,
        binding.tip,
        binding.need,
        binding.target,
        binding.revision,
        binding.base,
        binding.tip,
        context.source_hash,
    )
}

fn need_thread(ctx: &Ctx, need: &str) -> Result<Vec<Value>, VivariumError> {
    let command = format!(
        "need show {} --newest {} --json",
        shell_quote(need),
        usize::MAX
    );
    let output = ctx.vivi(&command, 60);
    if output.code != 0 {
        return Err(VivariumError::Message(format!(
            "cannot read full need thread {need}: {}",
            tail(&output.text, 300)
        )));
    }
    let start = output.text.find('[').ok_or_else(|| {
        VivariumError::Message(format!("need show returned no thread array for {need}"))
    })?;
    let messages = serde_json::from_str::<Vec<Value>>(&output.text[start..]).map_err(|error| {
        VivariumError::Message(format!(
            "need show returned invalid thread JSON for {need}: {error}"
        ))
    })?;
    if messages.is_empty()
        || !messages
            .iter()
            .any(|message| message["handle"].as_str() == Some(need))
    {
        return Err(VivariumError::Message(format!(
            "need show returned no root message for {need}"
        )));
    }
    Ok(messages)
}

fn validate_thread_revision(
    ctx: &Ctx,
    need: &str,
    expected: Option<&str>,
    messages: &[Value],
) -> Result<String, VivariumError> {
    let current = source_thread_hash(ctx, need, messages)?;
    let matches_plan = match expected {
        Some(hash) if hash.starts_with("v3:") => hash == current,
        Some(hash) if hash.starts_with("v2:") => {
            legacy_operator_thread_hash(ctx, messages)? == hash
        }
        _ => {
            return Err(VivariumError::Config(
                "need plan has no supported thread fingerprint; wait for amendment detection"
                    .into(),
            ));
        }
    };
    if matches_plan {
        Ok(current)
    } else {
        Err(VivariumError::Config(
            "need thread changed after its current plan revision; replan before auditing".into(),
        ))
    }
}

/// Current thread identity used by new audit dispatches and freshness checks.
fn source_thread_hash(ctx: &Ctx, root: &str, messages: &[Value]) -> Result<String, VivariumError> {
    crate::pump::amend::source_thread_hash(ctx, root, messages).ok_or_else(|| {
        VivariumError::Message(
            "need thread is missing a stable message id for its source fingerprint".into(),
        )
    })
}

/// Exact v2 plan-hash compatibility; new audit jobs always bind the v3 hash.
fn legacy_operator_thread_hash(ctx: &Ctx, messages: &[Value]) -> Result<String, VivariumError> {
    let mut hasher = Sha256::new();
    for message in messages {
        let from = message["from"].as_str().unwrap_or("");
        let sender = if ctx.cfg.reply_from.contains('@') {
            from
        } else {
            from.split('@').next().unwrap_or(from)
        };
        if sender.eq_ignore_ascii_case(&ctx.cfg.reply_from) {
            continue;
        }
        hasher.update(message["message_id"].as_str().unwrap_or("").as_bytes());
        hasher.update(b"\n");
        hasher.update(message["body"].as_str().unwrap_or("").as_bytes());
        hasher.update(b"\n");
    }
    Ok(format!("v2:{}", hex::encode(hasher.finalize())))
}

fn thread_requirements(
    ctx: &Ctx,
    need: &str,
    messages: &[Value],
) -> Result<Vec<NeedAuditRequirement>, VivariumError> {
    let mut requirements = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if crate::pump::amend::is_generated_pump_reply(ctx, need, index, message) {
            continue;
        }
        let handle = message["handle"].as_str().unwrap_or("");
        if handle.is_empty() {
            return Err(VivariumError::Message(
                "need thread message is missing its handle".into(),
            ));
        }
        let body = message["body"].as_str().unwrap_or("");
        let mut ordinal = 0usize;
        for line in body.lines() {
            let Some(text) = line.strip_prefix("done_when:") else {
                continue;
            };
            let text = text.trim();
            if text.is_empty() {
                return Err(VivariumError::Config(format!(
                    "need requirement in message {handle} is empty"
                )));
            }
            ordinal += 1;
            requirements.push(NeedAuditRequirement {
                id: format!("{handle}-r{ordinal}"),
                text: text.to_owned(),
            });
        }
    }
    if requirements.is_empty() {
        requirements = whole_message_requirements(ctx, need, messages);
    }
    validate_requirement_specs(&requirements).map_err(VivariumError::Config)?;
    Ok(requirements)
}

/// The longest message text one fallback requirement carries.
const FALLBACK_REQUIREMENT_CHARS: usize = 4000;

/// Requirements for a need that declares no `done_when:` lines: each source
/// message's own text is one requirement, so the audit checks the delivered
/// work against what the need actually asked for.
pub fn whole_message_requirements(
    ctx: &Ctx,
    need: &str,
    messages: &[Value],
) -> Vec<NeedAuditRequirement> {
    messages
        .iter()
        .enumerate()
        .filter(|(index, message)| {
            !crate::pump::amend::is_generated_pump_reply(ctx, need, *index, message)
        })
        .filter_map(|(_, message)| {
            let handle = message["handle"].as_str().filter(|h| !h.is_empty())?;
            let body = message["body"].as_str().unwrap_or("").trim();
            if body.is_empty() {
                return None;
            }
            let text: String = body.chars().take(FALLBACK_REQUIREMENT_CHARS).collect();
            Some(NeedAuditRequirement {
                id: format!("{handle}-r0"),
                text: format!("Deliver what this message asks for:\n{text}"),
            })
        })
        .collect()
}

fn source_messages(ctx: &Ctx, root: &str, messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .enumerate()
        .filter(|(index, message)| {
            !crate::pump::amend::is_generated_pump_reply(ctx, root, *index, message)
        })
        .map(|(_, message)| {
            serde_json::json!({
                "handle": message["handle"].as_str().unwrap_or(""),
                "message_id": message["message_id"].as_str().unwrap_or(""),
                "from": message["from"].as_str().unwrap_or(""),
                "subject": message["subject"].as_str().unwrap_or(""),
                "body": message["body"].as_str().unwrap_or(""),
            })
        })
        .collect()
}

fn need_units(st: &State, binding: &NeedAuditBinding) -> Result<Vec<NeedAuditUnit>, VivariumError> {
    crate::pump::need::units_for(st, &binding.target, &binding.need)
        .into_iter()
        .filter(|id| {
            st.units
                .get(id)
                .is_some_and(|unit| unit.revision == binding.revision)
        })
        .map(|id| {
            let unit = st.units.get(&id).ok_or_else(|| {
                VivariumError::Config(format!("frozen need unit {id} is missing from pump state"))
            })?;
            if unit.need != binding.need || unit.target_id() != binding.target {
                return Err(VivariumError::Config(format!(
                    "frozen need unit {id} has the wrong need or target"
                )));
            }
            Ok(NeedAuditUnit {
                id,
                title: unit.title.clone(),
                brief: unit.brief.clone(),
                status: unit.status.as_str().to_owned(),
                write_scope: unit.write_scope.clone(),
                proof: unit.proof.clone(),
                branch: unit.branch.clone(),
                tip: unit.commit.clone(),
            })
        })
        .collect()
}

fn current_plan(
    binding: &NeedAuditBinding,
    record: &crate::pump::state::NeedRecord,
    units: &[NeedAuditUnit],
) -> Result<String, VivariumError> {
    serde_json::to_string_pretty(&serde_json::json!({
        "need": binding.need,
        "target": binding.target,
        "revision": binding.revision,
        "verdict": record.verdict,
        "reason": record.reason,
        "units": units,
    }))
    .map_err(|error| VivariumError::Message(format!("encode current need plan: {error}")))
}

pub(super) fn changed_paths(
    ctx: &Ctx,
    base: &str,
    tip: &str,
) -> Result<Vec<String>, VivariumError> {
    let ancestry = ctx.git(
        &format!(
            "merge-base --is-ancestor {} {}",
            shell_quote(base),
            shell_quote(tip)
        ),
        60,
    );
    if ancestry.code != 0 {
        return Err(VivariumError::Config(format!(
            "need audit range {base}..{tip} is not a verified ancestor range"
        )));
    }
    let output = ctx.git(
        &format!(
            "diff --name-only --no-renames -z {} {} --",
            shell_quote(base),
            shell_quote(tip)
        ),
        60,
    );
    if output.code != 0 {
        return Err(VivariumError::Message(format!(
            "cannot inventory need audit range {base}..{tip}: {}",
            tail(&output.text, 300)
        )));
    }
    let mut paths: Vec<String> = output
        .text
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Verify the remote's exact frozen ref and fetch only that ref's commit
/// metadata when the target checkout is cold. This never checks out a tree or
/// updates FETCH_HEAD, remote-tracking refs, or the working branch.
pub(super) fn ensure_audit_range_objects(
    ctx: &Ctx,
    st: &State,
    binding: &NeedAuditBinding,
    audit_base: &str,
) -> Result<(), VivariumError> {
    ensure_current_binding(st, binding)?;
    validate_review_base(st, binding, audit_base)?;
    let branch = crate::pump::need::get(st, &binding.target, &binding.need)?
        .ok_or_else(|| VivariumError::Config("frozen need branch record is missing".into()))?;
    let expected_branch = crate::pump::need::need_branch(&binding.need);
    if branch.branch != expected_branch {
        return Err(VivariumError::Config(format!(
            "need audit branch {} does not match its owned ref {expected_branch}",
            branch.branch
        )));
    }

    let source_ref = format!("refs/heads/{}", branch.branch);
    let remote_tip = remote_ref_tip(ctx, &source_ref)?;
    if remote_tip != binding.tip {
        return Err(VivariumError::Config(format!(
            "need audit remote ref moved from frozen tip {} to {remote_tip}",
            binding.tip
        )));
    }
    if audit_objects_exist(ctx, binding)? {
        return Ok(());
    }
    fetch_audit_metadata(ctx, binding, &source_ref)
}

fn fetch_audit_metadata(
    ctx: &Ctx,
    binding: &NeedAuditBinding,
    source_ref: &str,
) -> Result<(), VivariumError> {
    let destination_ref = audit_metadata_ref(binding);
    let existing = ctx.git(
        &format!(
            "rev-parse --verify {}",
            shell_quote(&format!("{destination_ref}^{{commit}}"))
        ),
        15,
    );
    if existing.code == 0 && existing.text.trim() != binding.tip {
        return Err(VivariumError::Config(
            "need audit metadata ref already names a different commit".into(),
        ));
    }
    let refspec = format!("{source_ref}:{destination_ref}");
    let fetched = ctx.git(
        &format!(
            "fetch --quiet --no-tags --no-write-fetch-head --no-auto-gc --refmap= {} {}",
            shell_quote(&ctx.cfg.remote),
            shell_quote(&refspec)
        ),
        60,
    );
    if fetched.code != 0 {
        return Err(VivariumError::Message(format!(
            "cannot fetch frozen need audit metadata: {}",
            tail(&fetched.text, 300)
        )));
    }
    let fetched_tip = ctx.git(
        &format!(
            "rev-parse --verify {}",
            shell_quote(&format!("{destination_ref}^{{commit}}"))
        ),
        15,
    );
    if fetched_tip.code != 0 || fetched_tip.text.trim() != binding.tip {
        return Err(VivariumError::Config(
            "fetched need audit metadata did not resolve to the frozen tip".into(),
        ));
    }
    if !audit_objects_exist(ctx, binding)? {
        return Err(VivariumError::Config(
            "frozen need audit range objects are unavailable after metadata fetch".into(),
        ));
    }
    Ok(())
}

fn remote_ref_tip(ctx: &Ctx, reference: &str) -> Result<String, VivariumError> {
    let output = ctx.git(
        &format!(
            "ls-remote --exit-code {} {}",
            shell_quote(&ctx.cfg.remote),
            shell_quote(reference)
        ),
        30,
    );
    if output.code != 0 {
        return Err(VivariumError::Message(format!(
            "cannot verify frozen need branch ref {reference}: {}",
            tail(&output.text, 300)
        )));
    }
    let mut tip = None;
    for line in output.text.lines() {
        let Some((sha, name)) = line.split_once('\t') else {
            continue;
        };
        if name != reference {
            continue;
        }
        if !is_full_commit_sha(sha) || tip.is_some() {
            return Err(VivariumError::Config(format!(
                "remote returned a malformed or duplicate frozen need ref {reference}"
            )));
        }
        tip = Some(sha);
    }
    let Some(tip) = tip else {
        return Err(VivariumError::Config(format!(
            "remote did not advertise the exact frozen need ref {reference}"
        )));
    };
    Ok(tip.to_owned())
}

fn audit_objects_exist(ctx: &Ctx, binding: &NeedAuditBinding) -> Result<bool, VivariumError> {
    for sha in [&binding.tip, &binding.base] {
        let object = format!("{sha}^{{commit}}");
        let output = ctx.git(&format!("cat-file -e {}", shell_quote(&object)), 15);
        if output.code != 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn audit_metadata_ref(binding: &NeedAuditBinding) -> String {
    let mut hasher = Sha256::new();
    for field in [
        binding.target.as_str(),
        binding.need.as_str(),
        binding.tip.as_str(),
    ] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    format!("refs/pump/need-audit/{}", hex::encode(hasher.finalize()))
}

fn validate_review_base(
    st: &State,
    binding: &NeedAuditBinding,
    audit_base: &str,
) -> Result<(), VivariumError> {
    if !is_full_commit_sha(audit_base) {
        return Err(VivariumError::Config(
            "need audit requires a full review-range base SHA".into(),
        ));
    }
    let branch = crate::pump::need::get(st, &binding.target, &binding.need)?
        .ok_or_else(|| VivariumError::Config("frozen need branch record is missing".into()))?;
    if branch.audit_base_sha != audit_base {
        return Err(VivariumError::Config(
            "need audit review base does not match the frozen branch's authorized range".into(),
        ));
    }
    Ok(())
}

fn validate_context(
    binding: &NeedAuditBinding,
    audit_base: &str,
    round: u32,
    context: &NeedAuditContext,
) -> Result<(), VivariumError> {
    validate_binding(binding).map_err(VivariumError::Config)?;
    if !is_full_commit_sha(audit_base) || round == 0 {
        return Err(VivariumError::Config(
            "need audit requires a full review-range SHA and a positive round".into(),
        ));
    }
    if round > 1 && context.review_context.trim().is_empty() {
        return Err(VivariumError::Config(
            "repair-only need audit requires prior finding and blind-spot context".into(),
        ));
    }
    if context.original_need_thread.trim().is_empty() || context.current_plan.trim().is_empty() {
        return Err(VivariumError::Config(
            "need audit requires the original need thread and current plan".into(),
        ));
    }
    validate_requirement_specs(&context.requirements).map_err(VivariumError::Config)?;
    if context.units.iter().any(|unit| unit.id.trim().is_empty())
        || context
            .changed_paths
            .iter()
            .any(|path| path.trim().is_empty())
    {
        return Err(VivariumError::Config(
            "need audit packet contains an empty unit id or changed path".into(),
        ));
    }
    Ok(())
}

fn ensure_current_binding(st: &State, expected: &NeedAuditBinding) -> Result<(), VivariumError> {
    let current =
        crate::pump::need::binding_for(st, expected.target.as_str(), expected.need.as_str())?;
    if current.as_ref() == Some(expected) {
        Ok(())
    } else {
        Err(VivariumError::Config(format!(
            "need {} on target {} is not frozen at the requested revision and tip",
            expected.need, expected.target
        )))
    }
}

pub(super) fn validate_binding(binding: &NeedAuditBinding) -> Result<(), String> {
    if binding.need.trim().is_empty() || binding.target.trim().is_empty() || binding.revision == 0 {
        return Err(String::from("need audit binding is incomplete"));
    }
    if !is_full_commit_sha(&binding.base) || !is_full_commit_sha(&binding.tip) {
        return Err(String::from(
            "need audit binding requires full base and tip SHAs",
        ));
    }
    Ok(())
}

/// Whether `sha` is a canonical lowercase full Git object ID.
#[must_use]
pub fn is_full_commit_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64)
        && sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn validate_requirement_specs(
    requirements: &[NeedAuditRequirement],
) -> Result<(), String> {
    if requirements.is_empty() {
        return Err(String::from("need audit has no original requirements"));
    }
    let mut ids = std::collections::BTreeSet::new();
    if requirements.iter().any(|requirement| {
        requirement.id.trim().is_empty()
            || requirement.text.trim().is_empty()
            || !ids.insert(requirement.id.as_str())
    }) {
        return Err(String::from(
            "need audit requirements have empty or duplicate identities",
        ));
    }
    Ok(())
}

fn tail(text: &str, count: usize) -> String {
    let length = text.chars().count();
    text.chars().skip(length.saturating_sub(count)).collect()
}
