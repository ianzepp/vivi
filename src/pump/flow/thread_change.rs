//! Thread change: a reply on a need's thread while a build run is live.
//!
//! A reply from anyone but the pump identity, newer than the run's start post,
//! is the operator changing the question. In a live or parked run that has not
//! started `merge-main` or a merge, the run restarts at `plan` (the prior
//! branch stays readable). In a locked run, or once a merge-main or merge step
//! has started, the pump raises `flow-thread-changed` once and restarts
//! nothing: the merge is under way and the operator decides. No path holds a
//! change in silence.
//!
//! Assumption: a thread date has one-second resolution, so a reply in the same
//! second as the run's start post is not newer. Lifecycle events, pump record
//! letters and action letters are not replies.

use std::collections::BTreeSet;

use chrono::DateTime;

use crate::error::VivariumError;
use crate::mailspace::{Mailspace, MailspaceThreadMessage, Recipient};
use crate::pump::flow::advance::restart::restart;
use crate::pump::flow::def::{FlowDef, FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::run_state;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::tick::raise_once;
use crate::pump::host::Ctx;
use crate::pump::jobs::JobStore;
use crate::pump::record::open_mailspace;
use crate::pump::store::PumpStore;

/// Exception kind: a reply arrived on a run that is too far along to restart.
pub const THREAD_CHANGED: &str = "flow-thread-changed";

/// Exception kind: the thread check for a run could not be made (an
/// unparsable date, or the check itself failed).
pub const THREAD_CHECK_FAILED: &str = "flow-thread-check-failed";

/// Registry hook: the thread-change phase.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.phase(Box::new(ThreadChange));
}

/// Watches the thread of every live build run.
struct ThreadChange;

impl<S, M: FlowMail> TickPhase<S, M> for ThreadChange {
    fn name(&self) -> &'static str {
        "thread-change"
    }

    fn run(
        &self,
        ctx: &Ctx,
        _st: &mut S,
        store: &PumpStore,
        mail: &mut M,
    ) -> Result<(), VivariumError> {
        let runs: Vec<RunRow> = store
            .runs_in_state(run_state::LIVE)?
            .into_iter()
            .filter(|run| run.kind == FlowKind::Build.name())
            .collect();
        if runs.is_empty() {
            return Ok(());
        }
        let Some(mailspace) = open_mailspace(ctx) else {
            return Ok(());
        };
        let def = load(FlowKind::Build)?;
        for run in &runs {
            if let Err(error) = check_run(ctx, store, mail, &mailspace, &def, run) {
                let text = format!(
                    "need {} run r{}: the thread-change check failed and the run is skipped: \
                     {error}",
                    run.need, run.run
                );
                ctx.log(&format!(
                    "thread-change: {} r{} failed: {error}",
                    run.need, run.run
                ));
                raise_once(
                    ctx,
                    THREAD_CHECK_FAILED,
                    &format!("{} r{}", run.need, run.run),
                    &text,
                );
            }
        }
        Ok(())
    }
}

/// Look for a change on one run's thread and route the first one found.
fn check_run<M: FlowMail>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    mailspace: &Mailspace,
    def: &FlowDef,
    run: &RunRow,
) -> Result<(), VivariumError> {
    let thread = mailspace.thread(&run.need, false, usize::MAX, usize::MAX)?;
    let Some(change) = change_since_start(ctx, mailspace, &thread, run)? else {
        return Ok(());
    };
    let rows = store.steps_of(&run.need, run.run)?;
    if too_far_along(def, &rows) {
        let unit = format!("{} r{} {}", run.need, run.run, change.message_id);
        let text = format!(
            "need {} run r{}: a reply from {} arrived on the thread ({}) while the run holds \
             a lock or has started a merge; no restart. Operator: replan or answer the step",
            run.need, run.run, change.from, change.message_id
        );
        raise_once(ctx, THREAD_CHANGED, &unit, &text);
        return Ok(());
    }
    ctx.log(&format!(
        "thread-change: {} r{} restarts at plan on a reply from {} ({})",
        run.need, run.run, change.from, change.message_id
    ));
    restart(ctx, store, mail, run, "plan")?;
    Ok(())
}

/// The first reply on `thread` newer than the run's start post that is neither
/// the pump's, an event, nor a pump record or action letter. A date that does
/// not parse is an error naming the date text.
fn change_since_start<'a>(
    ctx: &Ctx,
    mailspace: &Mailspace,
    thread: &'a [MailspaceThreadMessage],
    run: &RunRow,
) -> Result<Option<&'a MailspaceThreadMessage>, VivariumError> {
    let pump = ctx.cfg.reply_from.as_str();
    let is_pump = |m: &MailspaceThreadMessage| matches!(mailspace.classify_recipient(&m.from), Ok(Recipient::Local(id)) if id == pump);
    let start = format!("pump: run r{} started", run.run);
    let Some(started) = thread
        .iter()
        .find(|m| is_pump(m) && m.event.is_none() && m.body.trim_start().starts_with(&start))
    else {
        return Ok(None);
    };
    let at = epoch(&started.date)?;
    for message in thread {
        if message.event.is_some() || is_pump(message) || is_pump_letter(message) {
            continue;
        }
        if epoch(&message.date)? > at {
            return Ok(Some(message));
        }
    }
    Ok(None)
}

/// A pump record or a posted action letter, never a reply on the thread.
fn is_pump_letter(message: &MailspaceThreadMessage) -> bool {
    message.kind.as_deref() == Some("pump")
        || (message.subject.starts_with("pump ") && message.subject.ends_with(" action"))
}

fn epoch(date: &str) -> Result<i64, VivariumError> {
    DateTime::parse_from_rfc3339(date)
        .map(|parsed| parsed.timestamp())
        .map_err(|error| VivariumError::Parse(format!("thread date {date:?}: {error}")))
}

/// True when the run waits at a lock or any merge step has begun.
fn too_far_along(def: &FlowDef, rows: &[StepRow]) -> bool {
    let merges = nodes_of(def, |c| {
        matches!(c, NodeClass::Merge | NodeClass::MergeMain)
    });
    let locks = nodes_of(def, |c| matches!(c, NodeClass::Lock));
    rows.iter().any(|row| {
        merges.contains(row.node.as_str())
            || (locks.contains(row.node.as_str()) && row.state == StepState::Waiting)
    })
}

fn nodes_of(def: &FlowDef, keep: impl Fn(&NodeClass) -> bool) -> BTreeSet<&str> {
    def.nodes
        .iter()
        .filter(|node| keep(&node.class))
        .map(|node| node.id.as_str())
        .collect()
}

#[cfg(test)]
#[path = "thread_change_test.rs"]
mod tests;
