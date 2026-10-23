//! Pruning a landed need's branches, one cleanup job per ref.
//!
//! The landing terminal has no job store, so a tick phase over `done` runs
//! enqueues the cleanup jobs instead: when a need's last run is done and its
//! branch row is not yet `pruned`, every ref the need owns on the row's target
//! gets one `prune-<need>-<ref>` job, and the row becomes `pruned`. A cleanup
//! job that fails raises `flow-prune-failed` once; it is never retried and
//! never reopens anything.

use std::collections::BTreeMap;

use crate::error::VivariumError;
use crate::pump::flow::fanout::is_slice_row;
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::RunRow;
use crate::pump::flow::need_branch::{self, DELETING, PRUNED, RENAMING};
use crate::pump::flow::tick::raise_once;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::jobs::{self, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{NeedBranchRow, PumpStore};

/// Exception kind of a cleanup job that did not delete its ref.
pub const PRUNE_FAILED: &str = "flow-prune-failed";

/// Id prefix of every cleanup job.
const JOB_PREFIX: &str = "prune-";

/// Run state of a landed run.
const RUN_DONE: &str = "done";

/// The ls-remote patterns that cover every namespace a need can own.
const NAMESPACES: [&str; 2] = ["refs/heads/vivi/*", "refs/heads/pump/merge-main/*"];

struct Prune;

impl<S: JobStore, M: FlowMail> TickPhase<S, M> for Prune {
    fn name(&self) -> &'static str {
        "prune"
    }

    fn run(
        &self,
        ctx: &Ctx,
        st: &mut S,
        store: &PumpStore,
        _mail: &mut M,
    ) -> Result<(), VivariumError> {
        report_failed(ctx, st);
        let mut seen: Vec<String> = Vec::new();
        for run in store.runs_in_state(RUN_DONE)? {
            if seen.contains(&run.need) {
                continue;
            }
            seen.push(run.need.clone());
            prune_need(ctx, st, store, &run.need)?;
        }
        Ok(())
    }
}

/// Registry hook: the prune phase.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.phase(Box::new(Prune));
}

/// Enqueue the cleanup of one landed need, then mark its row `pruned`.
fn prune_need(
    ctx: &Ctx,
    st: &mut impl JobStore,
    store: &PumpStore,
    need: &str,
) -> Result<(), VivariumError> {
    let Some(row) = store.need_branch(need)? else {
        return Ok(());
    };
    if row.status == PRUNED || row.status == RENAMING || row.status == DELETING {
        return Ok(());
    }
    let runs = store.runs_of(need)?;
    if runs.last().is_none_or(|run| run.state != RUN_DONE) {
        return Ok(());
    }
    let refs = owned_refs(ctx, store, &row, need, &runs)?;
    for (name, sha) in &refs {
        enqueue_cleanup(ctx, st, need, &row.target, name, sha)?;
    }
    store.set_need_branch_status(need, PRUNED)
}

/// The need's refs on its target: `(ref name without refs/heads/, sha)`.
fn owned_refs(
    ctx: &Ctx,
    store: &PumpStore,
    row: &NeedBranchRow,
    need: &str,
    runs: &[RunRow],
) -> Result<BTreeMap<String, String>, VivariumError> {
    let mut exact = vec![need_branch::branch(need)];
    exact.extend(row.priors.iter().cloned());
    let mut prefixes = Vec::new();
    let mut repair = false;
    for run in runs {
        repair |= run.kind == "repair";
        for step in store.steps_of(need, run.run)? {
            if is_slice_row(&step) {
                prefixes.push(format!("vivi/unit/{}-a", step.task));
            }
            prefixes.push(format!(
                "pump/merge-main/{}-merge-main-{}-",
                jobs::merge_main::ref_safe(&row.target),
                step.task
            ));
        }
    }
    if repair {
        prefixes.push(format!("vivi/repair/{need}-a"));
    }
    let listed = list_remote(ctx, &row.target)?;
    Ok(listed
        .into_iter()
        .filter(|(name, _)| {
            exact.contains(name)
                || prefixes.iter().any(|prefix| {
                    name.strip_prefix(prefix.as_str())
                        .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
                })
        })
        .collect())
}

/// Every branch under the namespaces a need can own, from one ls-remote.
fn list_remote(ctx: &Ctx, target: &str) -> Result<BTreeMap<String, String>, VivariumError> {
    let tctx = ctx
        .for_target(target)
        .ok_or_else(|| VivariumError::Config(format!("unknown target {target}")))?;
    let patterns: Vec<String> = NAMESPACES.iter().map(|p| shell_quote(p)).collect();
    let out = tctx.git(
        &format!(
            "ls-remote {} {}",
            shell_quote(&tctx.cfg.remote),
            patterns.join(" ")
        ),
        60,
    );
    if out.code != 0 {
        return Err(VivariumError::Other(format!(
            "ls-remote exit {}: {}",
            out.code,
            out.text.trim()
        )));
    }
    let mut refs = BTreeMap::new();
    for line in out.text.lines() {
        let mut parts = line.split_whitespace();
        if let (Some(sha), Some(name)) = (parts.next(), parts.next())
            && let Some(branch) = name.strip_prefix("refs/heads/")
        {
            refs.insert(branch.to_owned(), sha.to_owned());
        }
    }
    Ok(refs)
}

/// Queue the delete of one ref as a merge-role command job.
fn enqueue_cleanup(
    ctx: &Ctx,
    st: &mut impl JobStore,
    need: &str,
    target: &str,
    name: &str,
    sha: &str,
) -> Result<(), VivariumError> {
    let role = ctx
        .routing
        .merge()
        .filter(|role| role.status == "active" && ctx.routing.pool_of(role).is_some())
        .ok_or_else(|| {
            VivariumError::Config(format!(
                "cannot prune {name}: the merge role is missing or has no pool"
            ))
        })?;
    let id = format!("{JOB_PREFIX}{need}-{}", name.replace('/', "-"));
    let mut spec = JobSpec::new(
        id.clone(),
        "prune",
        role.name.clone(),
        format!("prune {name}"),
        format!(
            "Do not change the checkout. After your clean exit, the privileged pool runner deletes `{name}` only if it still matches `{sha}`."
        ),
    );
    spec.command = Some(String::from("true"));
    spec.target = Some(target.to_owned());
    spec.builtin_role = Some(String::from("merge"));
    spec.from_ref = Some(sha.to_owned());
    spec.branch = Some(format!("vivi/ref-delete/{id}"));
    spec.delete_ref = Some(format!("refs/heads/{name}"));
    spec.delete_expect = Some(sha.to_owned());
    jobs::enqueue(st, spec)
}

/// Raise once for each ended cleanup job that did not delete its ref, and mark
/// every ended cleanup job applied. A failed cleanup is never retried.
fn report_failed(ctx: &Ctx, st: &mut impl JobStore) {
    let ended: Vec<(String, Option<String>)> = st
        .jobs()
        .values()
        .filter(|job| job.id.starts_with(JOB_PREFIX) && job.applied_at.is_none())
        .filter(|job| job.status.is_terminal())
        .map(|job| (job.id.clone(), failure(job)))
        .collect();
    for (id, failed) in ended {
        if let Some(text) = failed {
            raise_once(ctx, PRUNE_FAILED, &id, &text);
        }
        jobs::mark_applied(st, &id, ctx.now());
    }
}

/// Why a cleanup job did not delete its ref, or `None` when it did.
fn failure(job: &PoolJob) -> Option<String> {
    let reference = job.delete_ref.as_deref().unwrap_or(&job.id);
    match job.status {
        JobStatus::Lost | JobStatus::Timeout => {
            Some(format!("cleanup of {reference} ended {:?}", job.status))
        }
        JobStatus::Finished => {
            let deleted = job
                .outcome
                .as_ref()
                .and_then(|outcome| outcome.runner_delete())
                .is_some_and(|receipt| receipt.succeeded());
            (!deleted).then(|| format!("cleanup of {reference} did not delete the ref"))
        }
        JobStatus::Pending | JobStatus::Dispatched => None,
    }
}

#[cfg(test)]
#[path = "prune_test.rs"]
mod tests;
