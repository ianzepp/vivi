//! Repairer step handler: each attempt works on a branch cut from the current
//! main tip, and the need gets a `need_branches` row so prune can find it.

use crate::error::VivariumError;
use crate::letter::records::NoteWord;
use crate::mailspace::shipped::REPAIRER_ROLE;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::need_branch;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler, role_job};
use crate::pump::flow::repair::{self, RepairRecord};
use crate::pump::flow::verify::remote_tip;
use crate::pump::jobs::JobStore;
use crate::pump::store::NeedBranchRow;

/// Starts a repairer attempt (`fix1`, `fix2`).
struct RepairerStart;

impl StepHandler for RepairerStart {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let need = &cx.run.need;
        let attempt = repair::attempt_of(&step.node).ok_or_else(|| {
            VivariumError::Parse(format!("repairer step node `{}` has no attempt", step.node))
        })?;
        let (_, body) = cx
            .mail
            .open_record(need)?
            .ok_or_else(|| VivariumError::Other(format!("need {need} has no open record")))?;
        let rec = repair::record(&body)?;
        let main_tip = main_tip(cx, &rec.target)?;
        record_branch(cx, &rec.target, &main_tip)?;
        let role = cx
            .ctx
            .routing
            .lookup(REPAIRER_ROLE)
            .ok_or_else(|| VivariumError::Config("no repairer role".to_owned()))?;
        let name = repair::branch(need, cx.run.run, attempt);
        let budget_secs = cx.ctx.cfg.repair_attempt_secs;
        let brief = brief(cx, &rec, &main_tip, &name, budget_secs, attempt)?;
        let mut spec = role_job(
            cx.ctx,
            cx.run,
            step,
            "repairer",
            RoleKind::Repairer,
            &role.name,
            &brief,
        );
        spec.target = Some(rec.target);
        spec.from_ref = Some(main_tip);
        spec.publish_ref = Some(format!("refs/heads/{name}"));
        spec.branch = Some(name);
        spec.read_only = false;
        spec.budget = budget_secs;
        Ok(StartAction::Job(Box::new(spec)))
    }
}

/// The target's main tip, read now: every attempt starts from the current one.
fn main_tip(cx: &StartCx<'_>, target: &str) -> Result<String, VivariumError> {
    let t = cx
        .ctx
        .for_target(target)
        .ok_or_else(|| VivariumError::Config(format!("unknown target {target}")))?;
    remote_tip(&t, &t.cfg.remote, &t.cfg.main)
        .map_err(VivariumError::Other)?
        .ok_or_else(|| {
            VivariumError::Other(format!(
                "main ref {} does not exist on {target}",
                t.cfg.main
            ))
        })
}

/// Write the need's `need_branches` row (live, this run, this base). The
/// branch stays the need's nominal name; repair branches are pruned by name.
fn record_branch(cx: &StartCx<'_>, target: &str, base: &str) -> Result<(), VivariumError> {
    let need = &cx.run.need;
    let priors = cx
        .store
        .need_branch(need)?
        .map(|row| row.priors)
        .unwrap_or_default();
    cx.store.upsert_need_branch(&NeedBranchRow::for_run(
        cx.run,
        need_branch::branch(need),
        Some(base.to_owned()),
        need_branch::LIVE,
        target,
        priors,
    ))
}

/// The worker brief of an attempt.
fn brief(
    cx: &StartCx<'_>,
    rec: &RepairRecord,
    main_tip: &str,
    name: &str,
    budget_secs: u64,
    attempt: u32,
) -> Result<String, VivariumError> {
    let tested = rec.tested.as_deref().unwrap_or("unknown");
    let mut text = format!(
        "Repair need: {need}\nTarget: {target}\nTested tip: {tested}\n\
         Current main tip: {main_tip}\nPublish to branch: {name}\n\n\
         Failing output:\n{tail}\n\n\
         Run the failing tests first.\n\
         Never weaken, delete, ignore or gate a test; fix the cause.\n\
         Time cap: {budget_secs} seconds for this attempt; no diff cap.\n\
         Reply with the repairer envelope (answer built or failed).\n",
        need = cx.run.need,
        target = rec.target,
        tail = rec.tail,
        budget_secs = budget_secs,
    );
    if attempt >= 2 {
        text.push_str(&prior_attempt(cx)?);
    }
    Ok(text)
}

/// The "Prior attempt" section: attempt-1 steps, its facts, and the newest
/// rung verdict reply.
fn prior_attempt(cx: &StartCx<'_>) -> Result<String, VivariumError> {
    let (need, run) = (&cx.run.need, cx.run.run);
    let mut text = String::from("\nPrior attempt:\n");
    let steps = cx.store.steps_of(need, run)?;
    let first: Vec<&StepRow> = steps
        .iter()
        .filter(|step| repair::attempt_of(&step.node) == Some(1))
        .collect();
    for step in &first {
        text.push_str(&format!(
            "- {}: {:?}, verdict {}",
            step.node,
            step.state,
            step.verdict.as_deref().unwrap_or("none")
        ));
        if let Some(row) = cx.store.step_payload(&step.task)? {
            if let Some(reason) = row.payload.get("reason").and_then(|v| v.as_str()) {
                text.push_str(&format!(": {reason}"));
            }
            if let Some(job) = row.payload.get("job").and_then(|v| v.as_str()) {
                text.push_str(&format!(" (job {job})"));
            }
        }
        text.push('\n');
    }
    match repair::latest_facts(cx.store, cx.mail, &cx.ctx.cfg.reply_from, need, run, 1)? {
        Some(facts) => text.push_str(&format!("Facts: {}\n", repair::facts_text(&facts))),
        None => text.push_str("Attempt 1 published nothing.\n"),
    }
    if let Some(rung) = first.iter().find(|s| s.node == repair::rung_node(1)) {
        let notes = cx
            .mail
            .notes_from(&rung.task, NoteWord::RepairRung, &cx.ctx.cfg.reply_from)?;
        if let Some((_, note)) = notes.last() {
            text.push_str(&format!("Rung verdict: {}\n", note.prose.trim()));
        }
    }
    Ok(text)
}

/// Registers this module's handlers.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(
        FlowKind::Repair,
        NodeClass::Repairer,
        Box::new(RepairerStart),
    );
}

#[cfg(test)]
#[path = "repair_start_test.rs"]
mod tests;
