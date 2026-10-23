//! Post-landing repair audit phase.
//!
//! The landing terminal has no job store, so a tick phase over `done` repair
//! runs enqueues one non-blocking auditor job per landed repair. The job never
//! holds anything. The `applied_actions` table is the durable done-marker: the
//! key `repair-audit:<need>:r<run>` is set once the audit is settled (or has
//! nothing to audit).

use serde_json::json;

use crate::error::VivariumError;
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::RunRow;
use crate::pump::flow::repair::{self, AttemptFacts};
use crate::pump::flow::tick::raise_once;
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobSpec, JobStore};
use crate::pump::store::PumpStore;

/// Exception kind of an audit that could not be queued or collected.
pub const REPAIR_AUDIT_FAILED: &str = "flow-repair-audit-failed";

/// Run state of a landed run.
const RUN_DONE: &str = "done";

/// Kind of a landed repair run.
const REPAIR_KIND: &str = "repair";

/// Kind label of an audit job.
pub(crate) const JOB_KIND: &str = "auditor";

/// The applied-action key of one run's audit.
pub(crate) fn run_key(need: &str, run: u32) -> String {
    format!("repair-audit:{need}:r{run}")
}

/// The id of one run's audit job.
pub(crate) fn job_id(need: &str, run: u32) -> String {
    format!("repair-audit-{need}-r{run}")
}

struct RepairAudit;

impl<S: JobStore, M: FlowMail> TickPhase<S, M> for RepairAudit {
    fn name(&self) -> &'static str {
        "repair-audit"
    }

    fn run(
        &self,
        ctx: &Ctx,
        st: &mut S,
        store: &PumpStore,
        mail: &mut M,
    ) -> Result<(), VivariumError> {
        for run in store.runs_in_state(RUN_DONE)? {
            if run.kind == REPAIR_KIND {
                enqueue(ctx, st, store, mail, &run)?;
            }
        }
        Ok(())
    }
}

/// Registers this module's handlers.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.phase(Box::new(RepairAudit));
}

/// Queue the audit of one landed repair run, unless it is already settled or
/// already queued.
fn enqueue<S: JobStore, M: FlowMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &M,
    run: &RunRow,
) -> Result<(), VivariumError> {
    let key = run_key(&run.need, run.run);
    let id = job_id(&run.need, run.run);
    if store.applied_action_ids()?.contains(&key) || st.jobs().contains_key(&id) {
        return Ok(());
    }
    let Some(facts) = published_facts(store, mail, run)? else {
        ctx.log(&format!(
            "repair audit: {} r{} published nothing; nothing to audit",
            run.need, run.run
        ));
        return settle(ctx, store, &key);
    };
    let Some(role) = ctx
        .routing
        .auditor()
        .filter(|role| role.status == "active" && ctx.routing.pool_of(role).is_some())
    else {
        let text = format!(
            "{} r{}: no active auditor role with a pool",
            run.need, run.run
        );
        raise_once(ctx, REPAIR_AUDIT_FAILED, &id, &text);
        return settle(ctx, store, &key);
    };
    let (target, brief) = brief(ctx, store, mail, run, &facts)?;
    let mut spec = JobSpec::new(
        id,
        JOB_KIND,
        role.name.clone(),
        format!("repair audit {} r{}", run.need, run.run),
        brief,
    );
    spec.target = target;
    spec.from_ref = Some(facts.commit.clone());
    spec.read_only = true;
    spec.budget = ctx.cfg.dispatch.budget;
    spec.payload = json!({
        "need": run.need,
        "run": run.run,
        "commit": facts.commit,
        "base": facts.base,
    });
    jobs::enqueue(st, spec)
}

/// Mark one audit key applied.
fn settle(ctx: &Ctx, store: &PumpStore, key: &str) -> Result<(), VivariumError> {
    store.mark_action_applied(key, ctx.now()).map(|_| ())
}

/// What the newest publishing attempt of a run published: attempt 2 first.
fn published_facts<M: FlowMail>(
    store: &PumpStore,
    mail: &M,
    run: &RunRow,
) -> Result<Option<AttemptFacts>, VivariumError> {
    for attempt in [2, 1] {
        if let Some(facts) = repair::latest_facts(store, mail, &run.need, run.run, attempt)? {
            return Ok(Some(facts));
        }
    }
    Ok(None)
}

/// The audit target and brief. The target is the record's, else the need's
/// branch row's (the record is closed once the repair has landed); the tested
/// tip and failing tail are named only while the record body is still readable.
fn brief<M: FlowMail>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &M,
    run: &RunRow,
    facts: &AttemptFacts,
) -> Result<(Option<String>, String), VivariumError> {
    let record = match mail.open_record(&run.need)? {
        Some((_, body)) => match repair::record(&body) {
            Ok(rec) => Some(rec),
            Err(error) => {
                ctx.log(&format!(
                    "repair audit: {} r{}: record unreadable ({error}); naming the range only",
                    run.need, run.run
                ));
                None
            }
        },
        None => None,
    };
    let target = match &record {
        Some(rec) => Some(rec.target.clone()),
        None => store.need_branch(&run.need)?.map(|row| row.target),
    };
    let mut text = format!(
        "Repair audit mode (auditor charter section 'Repair audit mode')\n\n\
         Need: {need}\nRun: r{run}\nTarget: {target}\nRange: {base}..{commit}\n",
        need = run.need,
        run = run.run,
        target = target.as_deref().unwrap_or("default"),
        base = facts.base,
        commit = facts.commit,
    );
    if let Some(rec) = &record {
        text.push_str(&format!(
            "Tested tip: {}\n\nFailing output:\n{}\n",
            rec.tested.as_deref().unwrap_or("unknown"),
            rec.tail
        ));
    }
    text.push_str(
        "\nCheck first for weakened, deleted, ignored or gated tests, then for \
         anything else the repair broke. Report findings in the auditor envelope.\n",
    );
    Ok((target, text))
}

#[cfg(test)]
#[path = "repair_audit_test.rs"]
mod tests;
