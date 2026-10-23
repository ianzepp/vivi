//! Post-landing repair audit phase.
//!
//! The landing terminal has no job store, so a tick phase over `done` repair
//! runs enqueues one non-blocking auditor job per landed repair. The job never
//! holds anything. The `applied_actions` table is the durable done-marker: the
//! key `repair-audit:<need>:r<run>` is set once the audit is settled (or has
//! nothing to audit).
//!
//! The collect half reads the terminal audit job: every `[[finding]]` of its
//! auditor envelope files one new need (a `weakened-test` finding at top
//! priority) and gets its own applied key, so a failed tick files only what is
//! still missing. Nothing is ever held or reopened.

use std::collections::BTreeSet;

use serde_json::json;

use crate::error::VivariumError;
use crate::pump::attempt_record::hand_message;
use crate::pump::envelope::review::Finding;
use crate::pump::envelope::roles::{RoleKind, parse_for};
use crate::pump::flow::fanout::VERDICT_MERGED;
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::run_state;
use crate::pump::flow::index::{RunRow, StepState};
use crate::pump::flow::repair::{self, AttemptFacts};
use crate::pump::flow::step_error::{self, STEP_ERROR_LIMIT};
use crate::pump::flow::tick::raise_once;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::jobs::{self, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::PumpStore;

/// Exception kind of an audit that could not be queued or collected.
pub const REPAIR_AUDIT_FAILED: &str = "flow-repair-audit-failed";

/// Exception kind of a finding given up after repeated failing ticks.
pub const REPAIR_AUDIT_GAVE_UP: &str = "flow-repair-audit-gave-up";

/// Kind of a landed repair run.
const REPAIR_KIND: &str = "repair";

/// Pump priority of a need filed for a weakened, deleted, ignored or gated test.
const WEAKENED_TEST_PRIORITY: i64 = 100;

/// Finding ids with this prefix name a weakened test.
const WEAKENED_PREFIX: &str = "weakened-test";

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
        let applied = store.applied_action_ids()?;
        let mut first_error = None;
        for run in store.runs_in_state(run_state::DONE)? {
            if run.kind != REPAIR_KIND || applied.contains(&run_key(&run.need, run.run)) {
                continue;
            }
            if let Err(error) = audit_run(ctx, st, store, mail, &run) {
                ctx.log(&format!(
                    "repair audit of {} r{} failed: {error}",
                    run.need, run.run
                ));
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

/// Collect or queue the audit of one landed repair run.
fn audit_run<S: JobStore, M: FlowMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &M,
    run: &RunRow,
) -> Result<(), VivariumError> {
    match st.jobs().get(&job_id(&run.need, run.run)).cloned() {
        Some(job) => collect(ctx, store, &job, run),
        None => enqueue(ctx, st, store, mail, run),
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
    let Some(attempt) = merged_attempt(store, run)? else {
        ctx.log(&format!(
            "repair audit: {} r{} has no merged merge step; nothing to audit",
            run.need, run.run
        ));
        return settle(ctx, store, &key);
    };
    let Some(facts) = published_facts(ctx, store, mail, run, attempt)? else {
        let text = format!(
            "{} r{}: merged attempt {attempt} has no attempt facts",
            run.need, run.run
        );
        raise_once(ctx, REPAIR_AUDIT_FAILED, &id, &text);
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

/// Settle one terminal audit job: file its findings, then mark the run. A job
/// that ended without a readable reply raises the alarm once and is marked.
fn collect(ctx: &Ctx, store: &PumpStore, job: &PoolJob, run: &RunRow) -> Result<(), VivariumError> {
    if !job.status.is_terminal() {
        return Ok(());
    }
    let key = run_key(&run.need, run.run);
    let findings = match findings_of(job) {
        Ok(findings) => findings,
        Err(error) => {
            let text = format!("{} r{}: {error}", run.need, run.run);
            raise_once(ctx, REPAIR_AUDIT_FAILED, &job.id, &text);
            return settle(ctx, store, &key);
        }
    };
    let applied = store.applied_action_ids()?;
    let mut complete = true;
    for finding in &findings {
        let fkey = format!("{key}:{}", finding.id);
        if applied.contains(&fkey) {
            continue;
        }
        match file(ctx, store, &applied, (run, job, finding), &fkey) {
            Ok(()) => {
                step_error::note_success(ctx, &fkey);
                settle(ctx, store, &fkey)?;
            }
            Err(error) => {
                let n = step_error::note_failure(ctx, &fkey);
                if n >= STEP_ERROR_LIMIT {
                    let text = format!(
                        "repair audit {} r{}: finding {}: expected a need filed; actual: {error} on {n} ticks; given up",
                        run.need, run.run, finding.id
                    );
                    raise_once(ctx, REPAIR_AUDIT_GAVE_UP, &fkey, &text);
                    settle(ctx, store, &fkey)?;
                } else {
                    let text =
                        format!("{} r{}: finding {}: {error}", run.need, run.run, finding.id);
                    raise_once(ctx, REPAIR_AUDIT_FAILED, &fkey, &text);
                    complete = false;
                }
            }
        }
    }
    if complete {
        settle(ctx, store, &key)?;
    }
    Ok(())
}

/// The findings of a finished audit job's envelope.
fn findings_of(job: &PoolJob) -> Result<Vec<Finding>, String> {
    let Some(outcome) = job
        .outcome
        .as_ref()
        .filter(|_| job.status == JobStatus::Finished)
    else {
        return Err(format!("audit job ended {:?} with no reply", job.status));
    };
    let parsed = parse_for(RoleKind::Auditor, &hand_message(&outcome.receipt))
        .map_err(|error| format!("auditor reply unreadable: {error}"))?;
    match parsed.front.get("finding") {
        Some(rows) => serde_json::from_value(rows.clone())
            .map_err(|error| format!("auditor findings unreadable: {error}")),
        None => Ok(Vec::new()),
    }
}

/// File one finding as a new need, give it a pump record, and raise a
/// weakened-test finding to top priority. The need handle is remembered under
/// `<fkey>:need:<handle>` so a retry after a later step failed does not file
/// the need twice.
fn file(
    ctx: &Ctx,
    store: &PumpStore,
    applied: &BTreeSet<String>,
    (run, job, finding): (&RunRow, &PoolJob, &Finding),
    fkey: &str,
) -> Result<(), String> {
    let prefix = format!("{fkey}:need:");
    let handle = match applied.iter().find_map(|id| id.strip_prefix(&prefix)) {
        Some(handle) => handle.to_owned(),
        None => {
            let handle = send_need(ctx, run, job, finding)?;
            store
                .mark_action_applied(&format!("{prefix}{handle}"), ctx.now())
                .map_err(|error| error.to_string())?;
            handle
        }
    };
    vivi_ok(ctx, &format!("pump send --need {}", shell_quote(&handle)))?;
    if finding.id.starts_with(WEAKENED_PREFIX) {
        let level = WEAKENED_TEST_PRIORITY;
        vivi_ok(
            ctx,
            &format!("pump priority set {} {level}", shell_quote(&handle)),
        )?;
    }
    Ok(())
}

/// `need send` for one finding; returns the new need handle.
fn send_need(ctx: &Ctx, run: &RunRow, job: &PoolJob, finding: &Finding) -> Result<String, String> {
    let subject = format!("repair audit {} r{}: {}", run.need, run.run, finding.id);
    let paths = if finding.paths.is_empty() {
        String::from("none named")
    } else {
        finding.paths.join(", ")
    };
    let body = format!(
        "{}\n\nPaths: {paths}\nAudited repair range: {}..{}\nRepair: {} r{}\n",
        finding.content,
        job.payload["base"].as_str().unwrap_or("unknown"),
        job.payload["commit"].as_str().unwrap_or("unknown"),
        run.need,
        run.run,
    );
    let from = shell_quote(&ctx.cfg.reply_from);
    let command = format!(
        "need send --from {from} --to {from} --subject {} --body {}",
        shell_quote(&subject),
        shell_quote(&body),
    );
    let out = ctx.vivi(&command, 60);
    if out.code != 0 {
        return Err(format!("need send failed: {}", out.text.trim()));
    }
    need_handle(&out.text).ok_or_else(|| format!("need send gave no handle: {}", out.text.trim()))
}

/// Run one `vivi` subcommand; an error names the command and its output.
fn vivi_ok(ctx: &Ctx, command: &str) -> Result<(), String> {
    let out = ctx.vivi(command, 60);
    if out.code == 0 {
        Ok(())
    } else {
        Err(format!("{command} failed: {}", out.text.trim()))
    }
}

/// The handle in `need send` output: the third token of the `created ` line
/// (`created <identity> <handle>`), else the second token of a `sent ` line.
fn need_handle(text: &str) -> Option<String> {
    let created = text
        .lines()
        .find(|line| line.starts_with("created "))
        .and_then(|line| line.split_whitespace().nth(2));
    let token = created.or_else(|| {
        text.lines()
            .find(|line| line.starts_with("sent "))
            .and_then(|line| line.split_whitespace().nth(1))
    });
    token.map(str::to_owned)
}

/// Mark one audit key applied.
fn settle(ctx: &Ctx, store: &PumpStore, key: &str) -> Result<(), VivariumError> {
    store.mark_action_applied(key, ctx.now()).map(|_| ())
}

/// The attempt whose merge step answered `merged`. A run with none landed
/// outside the flow (through `green`), so it has nothing of its own to audit.
fn merged_attempt(store: &PumpStore, run: &RunRow) -> Result<Option<u32>, VivariumError> {
    let steps = store.steps_of(&run.need, run.run)?;
    Ok(steps.iter().rev().find_map(|row| {
        let is_merge = matches!(row.node.as_str(), "merge1" | "merge2");
        let landed = row.state == StepState::Done && row.verdict.as_deref() == Some(VERDICT_MERGED);
        repair::attempt_of(&row.node).filter(|_| is_merge && landed)
    }))
}

/// What the merged attempt published.
fn published_facts<M: FlowMail>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &M,
    run: &RunRow,
    attempt: u32,
) -> Result<Option<AttemptFacts>, VivariumError> {
    let pump = &ctx.cfg.reply_from;
    repair::latest_facts(store, mail, pump, &run.need, run.run, attempt)
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
