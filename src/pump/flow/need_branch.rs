//! Need branches: names, the base recorded once per run, the tip and the
//! restart rule.
//!
//! There is one need branch per need (the `need_branches` table is keyed by
//! need), on one target: the row records the target of the need's first
//! slice, and every later read uses that target.
//!
//! A restarted run keeps the need branch for a `rework1` entry; every other
//! entry moves it to its prior name (publish the prior name, delete the old
//! name) and cuts a fresh one. [`BranchRestarts`] does that over several ticks.

use crate::error::VivariumError;
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::RunRow;
use crate::pump::flow::tick::raise_once;
use crate::pump::flow::verify::remote_tip;
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{NeedBranchRow, PumpStore};

/// No base yet for the row's run.
pub const FRESH: &str = "fresh";
/// A base is recorded for the row's run.
pub const LIVE: &str = "live";
/// The old branch is being moved to its prior name.
pub const RENAMING: &str = "renaming";
/// The old branch name is being deleted after the move.
pub const DELETING: &str = "deleting";
/// The need landed and its refs were cleaned up.
pub const PRUNED: &str = "pruned";

/// The restart entry that keeps the need branch.
const REWORK_ENTRY: &str = "rework1";
/// Exception kind for a need branch move that could not run or failed.
pub(crate) const RENAME_FAILED: &str = "flow-need-branch-rename-failed";

/// The need branch of `need`.
#[must_use]
pub fn branch(need: &str) -> String {
    format!("vivi/need/{need}")
}

/// The name a need branch is moved to when run `run` is restarted. One
/// segment after the `vivi/need-prior/` prefix, so the cleanup rule accepts it.
#[must_use]
pub fn prior(need: &str, run: u32) -> String {
    format!("vivi/need-prior/{need}-r{run}")
}

fn target_ctx(ctx: &Ctx, target: &str) -> Result<Ctx, VivariumError> {
    ctx.for_target(target)
        .ok_or_else(|| VivariumError::Config(format!("unknown target {target}")))
}

fn ls_remote(target: &Ctx, remote_ref: &str) -> Result<Option<String>, VivariumError> {
    remote_tip(target, &target.cfg.remote, remote_ref).map_err(VivariumError::Other)
}

/// Record the base of `need` for run `run`: the target's main tip, read once.
///
/// A `live` row for this run returns its base unchanged. A `renaming` or
/// `deleting` row is an error: the old branch is still being moved. Any other
/// row (or none) is written as `live` with the current main tip, keeping its
/// `priors`.
///
/// # Errors
/// Returns [`VivariumError`] for an unknown target, a moving old branch, a
/// missing main ref, or a failed ls-remote or store write.
pub fn record_base(
    ctx: &Ctx,
    store: &PumpStore,
    need: &str,
    run: u32,
    target: &str,
) -> Result<String, VivariumError> {
    let existing = store.need_branch(need)?;
    if let Some(row) = &existing {
        if row.status == RENAMING || row.status == DELETING {
            return Err(VivariumError::Other(format!(
                "need branch of {need} is {}: the old branch is still being moved",
                row.status
            )));
        }
        if row.status == LIVE
            && row.run == run
            && let Some(base) = &row.base
        {
            return Ok(base.clone());
        }
    }
    let target_ctx = target_ctx(ctx, target)?;
    let main = target_ctx.cfg.main.clone();
    let base = ls_remote(&target_ctx, &main)?.ok_or_else(|| {
        VivariumError::Other(format!("main ref {main} does not exist on target {target}"))
    })?;
    store.upsert_need_branch(&NeedBranchRow {
        need: need.to_owned(),
        branch: branch(need),
        base: Some(base.clone()),
        status: LIVE.to_owned(),
        run,
        target: target.to_owned(),
        priors: existing.map(|row| row.priors).unwrap_or_default(),
    })?;
    Ok(base)
}

/// The tip of the need branch: for a `live` row, the remote tip of
/// `vivi/need/<need>` on the row's target when the ref exists, else the base.
/// `None` for no row or any other status.
///
/// # Errors
/// Returns [`VivariumError`] for an unknown target or a failed ls-remote.
pub fn tip(ctx: &Ctx, store: &PumpStore, need: &str) -> Result<Option<String>, VivariumError> {
    let Some(row) = store.need_branch(need)? else {
        return Ok(None);
    };
    if row.status != LIVE {
        return Ok(None);
    }
    let target = target_ctx(ctx, &row.target)?;
    match ls_remote(&target, &format!("refs/heads/{}", row.branch))? {
        Some(sha) => Ok(Some(sha)),
        None => Ok(row.base),
    }
}

/// The restart rule as a tick phase: for each live run whose need branch row
/// belongs to an older run, keep the branch (`rework1`) or move it to its
/// prior name and reset the row.
struct BranchRestarts;

impl<S: JobStore, M: FlowMail> TickPhase<S, M> for BranchRestarts {
    fn name(&self) -> &'static str {
        "branch-restarts"
    }

    fn run(
        &self,
        ctx: &Ctx,
        st: &mut S,
        store: &PumpStore,
        _mail: &mut M,
    ) -> Result<(), VivariumError> {
        let mut first_error = None;
        for run in store.live_runs()? {
            let Some(row) = store.need_branch(&run.need)? else {
                continue;
            };
            if row.run >= run.run {
                continue;
            }
            if let Err(error) = restart(ctx, st, store, &run, row) {
                ctx.log(&format!(
                    "need branch restart of {} r{} failed: {error}",
                    run.need, run.run
                ));
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

fn restart<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    run: &RunRow,
    row: NeedBranchRow,
) -> Result<(), VivariumError> {
    match row.status.as_str() {
        LIVE if run.entry == REWORK_ENTRY => store.upsert_need_branch(&NeedBranchRow {
            run: run.run,
            ..row
        }),
        LIVE => begin_rename(ctx, st, store, run, &row),
        RENAMING => continue_rename(ctx, st, store, run, &row),
        DELETING => continue_delete(ctx, st, store, run, &row),
        _ => Ok(()),
    }
}

fn rename_id(need: &str, old_run: u32) -> String {
    format!("need-prior-{need}-r{old_run}")
}

fn cut_id(need: &str, old_run: u32) -> String {
    format!("need-cut-{need}-r{old_run}")
}

/// A no-model job under the merge role that runs `true` on `tip` of the row's
/// target; the caller sets what the runner publishes or deletes.
fn command_job(
    ctx: &Ctx,
    row: &NeedBranchRow,
    id: String,
    kind: &str,
    subject: &str,
    tip: &str,
    scratch: String,
) -> Option<JobSpec> {
    let role = ctx
        .routing
        .merge()
        .filter(|role| role.status == "active" && ctx.routing.pool_of(role).is_some())?;
    let mut spec = JobSpec::new(
        id,
        kind,
        role.name.clone(),
        subject,
        format!("{subject}.\n"),
    );
    spec.command = Some(String::from("true"));
    spec.target = Some(row.target.clone());
    spec.from_ref = Some(tip.to_owned());
    spec.branch = Some(scratch);
    Some(spec)
}

fn raise_failed(ctx: &Ctx, row: &NeedBranchRow, text: &str) {
    raise_once(
        ctx,
        RENAME_FAILED,
        &format!("{} r{}", row.need, row.run),
        text,
    );
}

fn no_merge_role(ctx: &Ctx, row: &NeedBranchRow) {
    raise_failed(
        ctx,
        row,
        &format!(
            "the merge role is missing, inactive or has no pool, so the need branch of {} cannot be moved",
            row.need
        ),
    );
}

/// The row is free for the new run: no base, `fresh`, `run` recorded.
fn reset(
    store: &PumpStore,
    row: &NeedBranchRow,
    run: u32,
    prior_name: Option<String>,
) -> Result<(), VivariumError> {
    let mut priors = row.priors.clone();
    priors.extend(prior_name.filter(|name| !priors.contains(name)));
    store.upsert_need_branch(&NeedBranchRow {
        need: row.need.clone(),
        branch: row.branch.clone(),
        base: None,
        status: FRESH.to_owned(),
        run,
        target: row.target.clone(),
        priors,
    })
}

fn old_tip(ctx: &Ctx, row: &NeedBranchRow) -> Result<Option<String>, VivariumError> {
    let target = target_ctx(ctx, &row.target)?;
    ls_remote(&target, &format!("refs/heads/{}", row.branch))
}

/// Step 2: publish the old tip under the prior name, or reset when no remote
/// branch exists.
fn begin_rename<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    run: &RunRow,
    row: &NeedBranchRow,
) -> Result<(), VivariumError> {
    let Some(tip) = old_tip(ctx, row)? else {
        return reset(store, row, run.run, None);
    };
    let name = prior(&row.need, row.run);
    let Some(mut spec) = command_job(
        ctx,
        row,
        rename_id(&row.need, row.run),
        "need-branch-rename",
        "move the need branch to its prior name",
        &tip,
        name.clone(),
    ) else {
        no_merge_role(ctx, row);
        return Ok(());
    };
    spec.publish_ref = Some(format!("refs/heads/{name}"));
    spec.publish_expect = Some(tip);
    jobs::enqueue(st, spec)?;
    store.set_need_branch_status(&row.need, RENAMING)
}

fn finished_with(job: &PoolJob, proof: impl Fn(&JobOutcome) -> bool) -> bool {
    job.status == JobStatus::Finished && job.outcome.as_ref().is_some_and(proof)
}

/// Step 3: when the publish succeeded, delete the old name.
fn continue_rename<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    run: &RunRow,
    row: &NeedBranchRow,
) -> Result<(), VivariumError> {
    let id = rename_id(&row.need, row.run);
    let Some(job) = st.jobs().get(&id).cloned() else {
        return begin_rename(ctx, st, store, run, row);
    };
    if !job.status.is_terminal() {
        return Ok(());
    }
    jobs::mark_applied(st, &id, ctx.now());
    let published = finished_with(&job, |out| {
        out.runner_publish().is_some_and(|p| p.succeeded())
    });
    let Some(tip) = job.from_ref.filter(|_| published) else {
        raise_failed(
            ctx,
            row,
            &format!(
                "moving the need branch of {} to its prior name failed",
                row.need
            ),
        );
        return Ok(());
    };
    enqueue_cut(ctx, st, store, row, &tip)
}

fn enqueue_cut<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    row: &NeedBranchRow,
    tip: &str,
) -> Result<(), VivariumError> {
    let id = cut_id(&row.need, row.run);
    let scratch = format!("vivi/ref-delete/{id}");
    let Some(mut spec) = command_job(
        ctx,
        row,
        id,
        "ref-delete",
        "delete the old need branch",
        tip,
        scratch,
    ) else {
        no_merge_role(ctx, row);
        return Ok(());
    };
    spec.delete_ref = Some(format!("refs/heads/{}", row.branch));
    spec.delete_expect = Some(tip.to_owned());
    jobs::enqueue(st, spec)?;
    store.set_need_branch_status(&row.need, DELETING)
}

/// Step 4: when the old name is deleted, the row is free for the new run.
fn continue_delete<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    run: &RunRow,
    row: &NeedBranchRow,
) -> Result<(), VivariumError> {
    let id = cut_id(&row.need, row.run);
    let Some(job) = st.jobs().get(&id).cloned() else {
        return match old_tip(ctx, row)? {
            Some(tip) => enqueue_cut(ctx, st, store, row, &tip),
            None => reset(store, row, run.run, Some(prior(&row.need, row.run))),
        };
    };
    if !job.status.is_terminal() {
        return Ok(());
    }
    jobs::mark_applied(st, &id, ctx.now());
    if finished_with(&job, |out| {
        out.runner_delete().is_some_and(|d| d.succeeded())
    }) {
        return reset(store, row, run.run, Some(prior(&row.need, row.run)));
    }
    raise_failed(
        ctx,
        row,
        &format!("deleting the old need branch of {} failed", row.need),
    );
    Ok(())
}

/// Registry hook: the need-branch restart phase.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.phase(Box::new(BranchRestarts));
}

#[cfg(test)]
#[path = "need_branch_test.rs"]
mod tests;
