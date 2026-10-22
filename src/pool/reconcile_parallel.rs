//! `vivi pool reconcile` over several pools at once.
//!
//! Reconciling a pool polls each of its open dispatches and lists its slots,
//! each a round trip to the pool's machine host. One pool never waits for
//! another: every pool is reconciled by [`work::reconcile`] on its own
//! thread, against a provider opened on that thread (a provider is not
//! shared between threads), and the reports are merged into the one a serial
//! pass over all the pools would have given.

use std::thread;

use super::collect::{Missing, Pacing, RoundLine};
use super::pools::PoolSet;
use super::provider::PoolProvider;
use super::record::OpenDispatch;
use super::slot::PoolId;
use super::work::{self, ReconcileReport};
use crate::error::VivariumError;
use crate::mailspace::Mailspace;

/// Reconcile every pool of `ids` concurrently, one thread per pool, and merge
/// the results. `open` makes a pool's machine host; it runs on the pool's own
/// thread. With `all`, the open dispatches of pools that are no longer
/// configured are closed as lost, as a serial reconcile does.
///
/// The merged report lists the round lines in the order the dispatches were
/// opened (the serial order), then each pool's slot cleanups in pool order,
/// then the idle note when nothing was open. Warnings and revoked keys follow
/// pool order.
///
/// # Errors
/// Returns the first error (in pool order) a pool's reconcile returned, after
/// every pool has finished.
pub fn reconcile_pools<F>(
    space: &Mailspace,
    ids: &[&PoolId],
    all: bool,
    pacing: &Pacing,
    open: &F,
) -> Result<ReconcileReport, VivariumError>
where
    F: Fn(&Mailspace, &PoolId) -> Result<Box<dyn PoolProvider>, VivariumError> + Sync,
{
    let opened = work::open_dispatches(space)?;
    let reports = thread::scope(|scope| {
        let tasks: Vec<_> = ids
            .iter()
            .map(|id| {
                let space = space.clone();
                scope.spawn(move || {
                    let provider = open(&space, id)?;
                    work::reconcile(&space, &PoolSet::single(id, provider.as_ref()), pacing)
                })
            })
            .collect();
        tasks
            .into_iter()
            .map(|task| {
                task.join().unwrap_or_else(|_| {
                    Err(VivariumError::Message(
                        "pool reconcile thread panicked".into(),
                    ))
                })
            })
            .collect::<Vec<_>>()
    });
    let mut parts = Vec::new();
    for report in reports {
        parts.push(report?);
    }
    let gone = if all {
        let gone: Vec<OpenDispatch> = opened
            .iter()
            .filter(|d| !ids.iter().any(|id| id.pool == d.record.pool))
            .cloned()
            .collect();
        close_gone(space, &gone, pacing)?
    } else {
        ReconcileReport::default()
    };
    Ok(merge(&opened, parts, gone))
}

/// Close the dispatches of pools that are no longer configured as lost.
fn close_gone(
    space: &Mailspace,
    gone: &[OpenDispatch],
    pacing: &Pacing,
) -> Result<ReconcileReport, VivariumError> {
    let mut report = ReconcileReport::default();
    if gone.is_empty() {
        return Ok(report);
    }
    let nobody = PoolSet {
        members: Vec::new(),
        all: true,
    };
    let lines = {
        let mut human = |line: &str| report.revoked.push(line.to_string());
        let mut record = work::recorder(space, &mut human);
        super::collect::run_round(
            &nobody,
            gone,
            work::epoch_secs(),
            pacing,
            Missing::Close,
            &mut record,
        )?
    };
    report.round = lines.len();
    report.items = lines;
    Ok(report)
}

/// Where `line`'s dispatch sits in `opened`, the order a serial round walks.
fn position(opened: &[OpenDispatch], line: &RoundLine) -> usize {
    opened
        .iter()
        .position(|d| {
            d.task == line.task && d.record.pool == line.pool && d.record.slot == line.slot
        })
        .unwrap_or(usize::MAX)
}

/// Merge per-pool reports (in pool order) and the gone-pool report into the
/// report one serial pass over every pool would have produced.
fn merge(
    opened: &[OpenDispatch],
    parts: Vec<ReconcileReport>,
    gone: ReconcileReport,
) -> ReconcileReport {
    let mut merged = ReconcileReport::default();
    let mut round: Vec<RoundLine> = Vec::new();
    let mut extras: Vec<RoundLine> = Vec::new();
    for part in parts.into_iter().chain(std::iter::once(gone)) {
        round.extend(part.items[..part.round].iter().cloned());
        extras.extend(part.items[part.round..].iter().cloned());
        merged.warnings.extend(part.warnings);
        merged.revoked.extend(part.revoked);
        merged.failures += part.failures;
        if merged.first_error.is_none() {
            merged.first_error = part.first_error;
        }
    }
    round.sort_by_key(|line| position(opened, line));
    if let Some(line) = round.iter().find(|line| line.state == "error") {
        merged.first_error = Some(line.detail.clone());
    }
    merged.idle = round.is_empty();
    merged.round = round.len();
    merged.items = round;
    merged.items.extend(extras);
    merged.out = merged.items.iter().map(work::line_text).collect();
    if merged.idle {
        merged.out.push("no open dispatches".into());
    }
    merged
}

#[cfg(test)]
#[path = "reconcile_parallel_test.rs"]
mod tests;
