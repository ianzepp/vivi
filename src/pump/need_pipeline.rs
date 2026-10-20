//! Orchestration of need acceptance; execution remains in durable pool jobs.

use super::host::Ctx;
use super::jobs::{self, PoolJob};
use super::need;
use super::need_audit;
use super::need_verdict;
use super::state::State;
use crate::VivariumError;

/// Route completed audit jobs without letting malformed or stale reports authorize work.
pub(crate) fn collect_audits(ctx: &Ctx, st: &mut State) -> Result<(), VivariumError> {
    let completed: Vec<PoolJob> = st
        .jobs
        .values()
        .filter(|job| {
            job.kind == "need-audit" && job.status.is_terminal() && job.applied_at.is_none()
        })
        .cloned()
        .collect();
    for job in completed {
        collect_audit(ctx, st, &job)?;
    }
    Ok(())
}

fn collect_audit(ctx: &Ctx, st: &mut State, job: &PoolJob) -> Result<(), VivariumError> {
    let expected = match need_audit::expectation_from_job(job) {
        Ok(expected) => expected,
        Err(error) => {
            ctx.exception("need-audit-identity", &job.id, &error);
            return Ok(());
        }
    };
    let binding = &expected.binding;
    if st.closed_needs.contains(&binding.need) {
        jobs::mark_applied(st, &job.id, ctx.now());
        ctx.log(&format!(
            "need audit {} retired: need was withdrawn",
            job.id
        ));
        return Ok(());
    }
    if need::binding_for(st, &binding.target, &binding.need)?.as_ref() != Some(binding) {
        jobs::mark_applied(st, &job.id, ctx.now());
        ctx.log(&format!("obsolete need audit retired {}", job.id));
        return Ok(());
    }
    let receipt = match need_audit::parse_job_receipt(job) {
        Ok(receipt) => receipt,
        Err(error) => {
            ctx.exception("need-audit-result", &job.id, &error);
            return Ok(());
        }
    };
    match need_verdict::route_receipt(ctx, st, &binding.target, &job.id, &receipt) {
        Ok(outcome) => {
            jobs::mark_applied(st, &job.id, ctx.now());
            ctx.log(&format!("need audit {} routed {outcome:?}", job.id));
        }
        Err(error) => ctx.exception("need-audit-route", &job.id, &error.to_string()),
    }
    Ok(())
}

/// Prepare first audits and repair reviews without duplicate jobs or policy in the pump.
pub(crate) fn dispatch_audits(ctx: &Ctx, st: &mut State) -> Result<Vec<String>, VivariumError> {
    let mut queued = Vec::new();
    for record in need::frozen(st)? {
        if st.closed_needs.contains(&record.need) {
            continue;
        }
        let Some(binding) = need::binding_for(st, &record.target, &record.need)? else {
            continue;
        };
        let verdict = need_verdict::state(st, &record.target, &record.need)?;
        let same_revision = verdict
            .original_binding
            .as_ref()
            .is_some_and(|old| old.revision == binding.revision);
        if verdict.hold.is_some() || (same_revision && verdict.recheck_base.is_some()) {
            continue;
        }
        if need_verdict::current_accepted_receipt(ctx, st, &record.need, &record.target).is_some() {
            continue;
        }
        match need_audit::dispatch_frozen(ctx, st, binding, &record.audit_base_sha, 1, "") {
            Ok(id) => queued.push(id),
            Err(error) => ctx.exception("need-audit-dispatch", &record.need, &error.to_string()),
        }
    }
    match need_verdict::dispatch_pending_reaudits(ctx, st) {
        Ok(ids) => queued.extend(ids),
        Err(error) => ctx.exception("need-repair-audit", "repair-dispatch", &error.to_string()),
    }
    Ok(queued)
}

/// Default-target unit counters advance only after verified need publication.
pub(crate) fn refresh_default_merges(st: &mut State) -> Result<(), VivariumError> {
    let mut integrated = 0u64;
    for unit in st
        .units
        .values()
        .filter(|unit| unit.target_id() == "default" && unit.status == super::state::Status::Merged)
    {
        let published = need::get(st, "default", &unit.need)?
            .is_none_or(|record| record.landing_receipt.is_some());
        if published {
            integrated += 1;
        }
    }
    st.merges = st.merges.max(integrated);
    Ok(())
}
