//! Need branches: names, the base recorded once per run, the tip and the
//! restart rule.
//!
//! There is one need branch per need (the `need_branches` table is keyed by
//! need), on one target: the row records the target of the need's first
//! slice, and every later read uses that target.
//!
//! A restarted build run adopts the live branch at every entry (`plan`,
//! `research`, `slice`, `rework1`): run ownership alone moves to the new run,
//! so the branch, its base, target, live status and priors — and with them the
//! merged slices of the prior run — stay reachable to the new plan, even
//! before the first merge when only a base exists. A build run that finds the
//! row mid-move (`renaming`/`deleting`) keeps the row and its recovery
//! evidence and raises `flow-need-branch-rename-failed` rather than queueing
//! more destructive work. Other kinds keep the archival rule: the branch moves
//! to its prior name (publish the prior name, delete the old name) and a fresh
//! branch is cut. [`BranchRestarts`] does that over several ticks.

use crate::error::VivariumError;
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::RunRow;
use crate::pump::flow::retained_work;
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

/// The build entries that adopt the prior run's branch at a restart.
const BUILD_ENTRIES: [&str; 4] = ["plan", "research", "slice", "rework1"];
/// Exception kind for a need branch move that could not run or failed.
pub(crate) const RENAME_FAILED: &str = "flow-need-branch-rename-failed";
/// Exception kind for retained merged work the remote no longer serves.
pub(crate) const WORK_UNREACHABLE: &str = "flow-need-work-unreachable";

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

/// Whether run `run` may take the need branch of `need` now: the verdict of
/// [`readiness`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Readiness {
    /// Nothing blocks the run: the base may be recorded.
    Ready,
    /// An older `live` row awaits adoption by the restart phase: an expected
    /// ownership wait, not a preparation error.
    AwaitsAdoption,
    /// The old branch is still being moved: blocked until the restart phase
    /// recovers it, which raises `flow-need-branch-rename-failed` when the
    /// move cannot run or fails.
    Moving,
    /// The row is recorded on another target.
    WrongTarget,
}

/// Classify whether run `run` may take the need branch of `need` on `target`.
///
/// One predicate for flow preparation and jobs release. It is pure: it adopts
/// nothing, moves nothing and raises nothing. [`Readiness::AwaitsAdoption`]
/// separates an expected ownership wait from a refusal without matching error
/// prose, and a `renaming`/`deleting` row stays blocked for explicit recovery.
#[must_use]
pub(crate) fn readiness(row: Option<&NeedBranchRow>, run: u32, target: &str) -> Readiness {
    let Some(row) = row else {
        return Readiness::Ready;
    };
    if row.status == RENAMING || row.status == DELETING {
        return Readiness::Moving;
    }
    if row.status == LIVE && row.run != run {
        return Readiness::AwaitsAdoption;
    }
    if row.target != target {
        return Readiness::WrongTarget;
    }
    Readiness::Ready
}

/// Record the base of `need` for run `run`: the target's main tip, read once.
///
/// A `live` row of this run on the row's target returns its base unchanged.
/// The stored row is never replaced by a later caller: a `renaming` or
/// `deleting` row is refused while the old branch is still being moved, an
/// older `live` row is refused until the restart phase adopts it, and a row
/// recorded on another target is refused. Every refusal leaves the stored row
/// unchanged. Any other row (or none) is written as `live` with the current
/// main tip, keeping its `priors`.
///
/// # Errors
/// Returns [`VivariumError`] for an unknown target, a moving old branch, an
/// unadopted older live row, a target mismatch, a missing main ref, or a
/// failed ls-remote or store write.
pub fn record_base(
    ctx: &Ctx,
    store: &PumpStore,
    need: &str,
    run: u32,
    target: &str,
) -> Result<String, VivariumError> {
    let existing = store.need_branch(need)?;
    if let Some(row) = &existing {
        match readiness(Some(row), run, target) {
            Readiness::Ready => {}
            Readiness::Moving => {
                return Err(VivariumError::Other(format!(
                    "need branch of {need} is {}: the old branch is still being moved",
                    row.status
                )));
            }
            Readiness::AwaitsAdoption => {
                return Err(VivariumError::Other(format!(
                    "need branch of {need} belongs to run {} and awaits adoption by run {run}",
                    row.run
                )));
            }
            Readiness::WrongTarget => {
                return Err(VivariumError::Config(format!(
                    "need branch of {need} is recorded on target {}, but run {run} targets {target}",
                    row.target
                )));
            }
        }
        if row.status == LIVE
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
    let run_row = store.run_row(need, run)?;
    store.upsert_need_branch(&NeedBranchRow::for_run(
        &run_row,
        branch(need),
        Some(base.clone()),
        LIVE,
        target,
        existing.map(|row| row.priors).unwrap_or_default(),
    ))?;
    Ok(base)
}

/// The checked resolution of a live need-branch row: whether the recorded
/// branch ref is present on the recorded target, and the SHA a worker should
/// check out — the remote tip when present, else the recorded base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NeedTip {
    /// The branch name recorded on the row.
    pub branch: String,
    /// The target recorded on the row.
    pub target: String,
    /// Whether the branch ref exists on the remote right now.
    pub on_remote: bool,
    /// The SHA to check out.
    pub checkout: Option<String>,
}

/// Resolve the tip of the need branch of `need` under the checked policy.
///
/// The recorded target is queried once. When the branch ref is present, its
/// tip is the checkout. When it is absent, the recorded base stands only
/// while no active retained merged work exists — merged work on a vanished
/// branch is unreachable, so it raises `flow-need-work-unreachable` instead
/// of falling back to the base or main. A query failure propagates; it is
/// never treated as an absent ref. `None` for no row or a non-live row, and
/// no branch row is created.
///
/// # Errors
/// Returns [`VivariumError`] for an unknown target, a failed ls-remote, an
/// unreadable retained history, or unreachable retained merged work.
pub(crate) fn checked_tip(
    ctx: &Ctx,
    store: &PumpStore,
    need: &str,
) -> Result<Option<NeedTip>, VivariumError> {
    let Some(row) = store.need_branch(need)? else {
        return Ok(None);
    };
    if row.status != LIVE {
        return Ok(None);
    }
    let target = target_ctx(ctx, &row.target)?;
    let resolved = match ls_remote(&target, &format!("refs/heads/{}", row.branch))? {
        Some(sha) => NeedTip {
            branch: row.branch.clone(),
            target: row.target.clone(),
            on_remote: true,
            checkout: Some(sha),
        },
        None => base_tip(ctx, store, &row)?,
    };
    Ok(Some(resolved))
}

/// The branch ref is absent: fall back to the recorded base, unless active
/// retained merged work exists — then the work is unreachable.
fn base_tip(ctx: &Ctx, store: &PumpStore, row: &NeedBranchRow) -> Result<NeedTip, VivariumError> {
    let history = retained_work::retained(store, row)?;
    if history.active_merged().next().is_some() {
        let unit = format!("{} r{}", row.need, row.run);
        raise_once(
            ctx,
            WORK_UNREACHABLE,
            &unit,
            &format!(
                "the need branch {} of {unit} is absent from target {} while retained merged work exists, so that work is unreachable",
                row.branch, row.target
            ),
        );
        return Err(VivariumError::Other(format!(
            "need branch {} of {unit} is absent while retained merged work exists",
            row.branch
        )));
    }
    Ok(NeedTip {
        branch: row.branch.clone(),
        target: row.target.clone(),
        on_remote: false,
        checkout: row.base.clone(),
    })
}

/// The tip of the need branch: for a `live` row, the remote tip of
/// `vivi/need/<need>` on the row's target when the ref exists, else the base
/// while no active retained merged work exists. Merged work on an absent
/// branch raises `flow-need-work-unreachable` instead of falling back.
/// `None` for no row or any other status.
///
/// # Errors
/// Returns [`VivariumError`] for an unknown target, a failed ls-remote, an
/// unreadable retained history, or unreachable retained merged work.
pub fn tip(ctx: &Ctx, store: &PumpStore, need: &str) -> Result<Option<String>, VivariumError> {
    Ok(checked_tip(ctx, store, need)?.and_then(|resolved| resolved.checkout))
}

/// The restart rule as a tick phase: for each live run, adopt the branch when
/// the run is a build run, surface a row stranded mid-move, and otherwise move
/// an older row to its prior name and reset it.
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
            if let Err(error) = restart_run(ctx, st, store, &run) {
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

/// Whether a build run adopts the prior run's branch: every build entry does,
/// so the merged slices of the prior run stay reachable to the new plan.
fn adopts(run: &RunRow) -> bool {
    run.kind == "build" && BUILD_ENTRIES.contains(&run.entry.as_str())
}

/// Adopt the branch for the new run: move run ownership only. Branch, base,
/// target, live status and priors stay as they are, even before the first
/// merge when only a base exists. No merge-role route, remote query or job is
/// involved, so repeated ticks are idempotent.
fn adopt(store: &PumpStore, run: &RunRow, row: &NeedBranchRow) -> Result<(), VivariumError> {
    store.upsert_need_branch(&NeedBranchRow::for_run(
        run,
        row.branch.clone(),
        row.base.clone(),
        &row.status,
        &row.target,
        row.priors.clone(),
    ))
}

/// Restart the need branch of one live run when its row belongs to an older
/// run, or surface a row stranded mid-move.
fn restart_run<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    run: &RunRow,
) -> Result<(), VivariumError> {
    let Some(row) = store.need_branch(&run.need)? else {
        return Ok(());
    };
    if (row.status == RENAMING || row.status == DELETING) && adopts(run) {
        // Stranded mid-move: keep the row and its recovery evidence, raise
        // once, queue nothing destructive — whatever run owns the row.
        raise_failed(
            ctx,
            &row,
            &format!(
                "the need branch of {} is stranded {} from run {}, so run {} cannot adopt it until the move is recovered",
                row.need, row.status, row.run, run.run
            ),
        );
        return Ok(());
    }
    if row.run >= run.run {
        return Ok(());
    }
    restart(ctx, st, store, run, row)
}

fn restart<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    run: &RunRow,
    row: NeedBranchRow,
) -> Result<(), VivariumError> {
    match row.status.as_str() {
        LIVE if adopts(run) => adopt(store, run, &row),
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
    run: &RunRow,
    prior_name: Option<String>,
) -> Result<(), VivariumError> {
    let mut priors = row.priors.clone();
    priors.extend(prior_name.filter(|name| !priors.contains(name)));
    store.upsert_need_branch(&NeedBranchRow::for_run(
        run,
        row.branch.clone(),
        None,
        FRESH,
        &row.target,
        priors,
    ))
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
        return reset(store, row, run, None);
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
            None => reset(store, row, run, Some(prior(&row.need, row.run))),
        };
    };
    if !job.status.is_terminal() {
        return Ok(());
    }
    jobs::mark_applied(st, &id, ctx.now());
    if finished_with(&job, |out| {
        out.runner_delete().is_some_and(|d| d.succeeded())
    }) {
        return reset(store, row, run, Some(prior(&row.need, row.run)));
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
