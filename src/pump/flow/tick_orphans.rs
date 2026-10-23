//! Tick checks for work no live run looks after: dispatched step jobs of runs
//! that are no longer live, and a fanout step with no open fanout row.

use super::{
    Ctx, FANOUT_UNBOUND, FlowMail, JobStatus, JobStore, NodeClass, OVERDUE_GRACE_SECS, PoolJob,
    PumpStore, RunCx, SENTINEL_NODE, STEP_OVERDUE, StepState, VivariumError, is_slice_row,
    orphan_state, raise_once, run_state,
};

/// Raise once for each dispatched step job past its budget plus grace whose
/// run is not live. The live run's own pass (`check_overdue`) covers the jobs
/// of live runs.
///
/// # Errors
/// Returns a [`VivariumError`] when the step index cannot be read.
pub(super) fn overdue_orphans<S: JobStore>(
    ctx: &Ctx,
    st: &S,
    store: &PumpStore,
) -> Result<(), VivariumError> {
    let now = ctx.now();
    let late: Vec<&PoolJob> = st
        .jobs()
        .values()
        .filter(|job| job.status == JobStatus::Dispatched)
        .filter(|job| {
            job.dispatched_at
                .is_some_and(|at| now > at + job.budget + OVERDUE_GRACE_SECS)
        })
        .collect();
    for job in late {
        let Some(row) = store.step_job(&job.id)? else {
            continue;
        };
        let (run, state) = orphan_state(store, &row)?;
        if state == run_state::LIVE {
            continue;
        }
        raise_once(
            ctx,
            STEP_OVERDUE,
            &job.id,
            &format!(
                "step {} task {} job {} pool {} dispatched at {}: budget {}s + {OVERDUE_GRACE_SECS}s grace passed; run r{run} is {state}, not live",
                row.node,
                row.task,
                job.id,
                job.pool.as_deref().unwrap_or("?"),
                job.dispatched_at.unwrap_or_default(),
                job.budget
            ),
        );
    }
    Ok(())
}

impl<S, M> RunCx<'_, S, M>
where
    S: JobStore,
    M: FlowMail,
{
    /// Raise once when the run's fanout step is running but no fanout row is
    /// open: `fanout::settle` then counts no slices and the step never
    /// settles.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the step index cannot be read.
    pub(super) fn check_fanout_bound(&self) -> Result<(), VivariumError> {
        if self
            .store
            .open_fanout(&self.run.need, self.run.run)?
            .is_some()
        {
            return Ok(());
        }
        for step in self.store.steps_of(&self.run.need, self.run.run)? {
            if step.state != StepState::Running
                || step.node == SENTINEL_NODE
                || is_slice_row(&step)
                || self.class_of(&step.node) != Some(NodeClass::Fanout)
            {
                continue;
            }
            raise_once(
                self.ctx,
                FANOUT_UNBOUND,
                &format!("{} r{} {}", self.run.need, self.run.run, step.task),
                &format!(
                    "need {} run r{}: fanout step {} task {} is running; expected an open fanout row; actual: none, so its slices never settle",
                    self.run.need, self.run.run, step.node, step.task
                ),
            );
        }
        Ok(())
    }
}
