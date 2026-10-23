//! Intake: start a run for each need with an open pump record.
//!
//! Each tick the phase resumes runs left `starting`, then ranks the needs that
//! have an open pump record and no run in flight, and starts as many as the
//! `[pump.flow] max_live_runs` cap allows, at most one repair-kind run per
//! target. Repair-kind runs neither count toward the cap nor wait for it. The addressee of the need plays no part.
//!
//! Assumption: the target of a repair-kind need is read from a `target:` line
//! of the open pump record body, which N6 writes together with the
//! `kind: repair` line.

use std::collections::BTreeMap;

use chrono::DateTime;

use crate::error::VivariumError;
use crate::pump::flow::advance::restart::RUN_RESTARTING;
use crate::pump::flow::advance::start::{RUN_STARTING, resume, start};
use crate::pump::flow::advance::terminal::RUN_FINISHING;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::kind;
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailReader, NeedStatus};
use crate::pump::flow::rank::need_key;
use crate::pump::flow::tick::{FLOW_INVALID, raise_once};
use crate::pump::host::Ctx;
use crate::pump::jobs::JobStore;
use crate::pump::store::PumpStore;

/// Exception kind: a run stayed `starting` too long.
pub const START_STUCK: &str = "flow-run-start-stuck";

/// Seconds a run may stay `starting` before the alarm.
const START_STUCK_SECS: u64 = 600;

/// Run states that occupy a slot under the cap or block a new run.
const RUN_LIVE: &str = "live";

/// The mark that keeps a need out of intake until N7 retires it.
const MARK_INVESTIGATE: &str = "investigate";

/// Registry hook: the intake phase.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.phase(Box::new(Intake));
}

/// An open-record need that may get a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub need: String,
    pub repair: bool,
    /// The repair target ("" when none or not a repair need).
    pub target: String,
    pub priority: i64,
    /// Record age key: the record's epoch seconds (older sorts first), or
    /// `u64::MAX` when the date does not parse.
    pub age: u64,
}

/// The needs to start, in rank order.
///
/// `active` is the number of non-repair runs already starting, live or
/// restarting; `busy_targets` the targets of repair-kind runs already in
/// flight. With a `cap` the selection stops picking ordinary candidates when
/// `active` plus the ordinary picks reach it. A repair-kind candidate is never
/// stopped by the cap and does not count toward it; one whose target is busy
/// (or already picked) is skipped.
#[must_use]
pub(crate) fn select(
    mut candidates: Vec<Candidate>,
    active: usize,
    cap: Option<usize>,
    busy_targets: &[String],
) -> Vec<String> {
    candidates.sort_by_key(|c| need_key(c.repair, c.priority, c.age, &c.need));
    let mut busy: Vec<String> = busy_targets.to_vec();
    let mut picked = Vec::new();
    let mut ordinary = 0;
    for candidate in candidates {
        if candidate.repair {
            if busy.contains(&candidate.target) {
                continue;
            }
            busy.push(candidate.target.clone());
        } else {
            if cap.is_some_and(|cap| active + ordinary >= cap) {
                break;
            }
            ordinary += 1;
        }
        picked.push(candidate.need);
    }
    picked
}

struct Intake;

impl<S: JobStore, M: FlowMail> TickPhase<S, M> for Intake {
    fn name(&self) -> &'static str {
        "intake"
    }

    fn run(
        &self,
        ctx: &Ctx,
        _st: &mut S,
        store: &PumpStore,
        mail: &mut M,
    ) -> Result<(), VivariumError> {
        resume_starting(ctx, store, mail);
        let (active, busy) = in_flight(store, mail)?;
        let cap = ctx
            .cfg
            .flow
            .max_live_runs
            .map(|n| usize::try_from(n).unwrap_or(usize::MAX));
        let found = candidates(ctx, store, mail)?;
        for need in select(found, active, cap, &busy) {
            start(ctx, store, mail, &need, None)?;
        }
        Ok(())
    }
}

/// Resume every `starting` run; past the stuck limit raise the alarm once.
fn resume_starting<M: FlowMail>(ctx: &Ctx, store: &PumpStore, mail: &mut M) {
    let runs = match store.runs_in_state(RUN_STARTING) {
        Ok(runs) => runs,
        Err(error) => {
            ctx.log(&format!("flow intake: cannot list starting runs: {error}"));
            return;
        }
    };
    for run in runs {
        let Err(error) = resume(ctx, store, mail, &run) else {
            continue;
        };
        let unit = format!("{} r{}", run.need, run.run);
        ctx.log(&format!("flow intake: resume of {unit} failed: {error}"));
        let since = match store.run_state_at(&run.need, run.run) {
            Ok(Some(at)) => u64::try_from(at).unwrap_or(u64::MAX),
            _ => u64::MAX,
        };
        if since != u64::MAX && ctx.now().saturating_sub(since) > START_STUCK_SECS {
            raise_once(
                ctx,
                START_STUCK,
                &unit,
                &format!(
                    "need {} run {} is stuck starting: {error}",
                    run.need, run.run
                ),
            );
        }
    }
}

/// The count of non-repair runs under the cap and the targets of repair-kind
/// runs in flight (repair runs use no cap slot).
fn in_flight<M: FlowMail>(
    store: &PumpStore,
    mail: &M,
) -> Result<(usize, Vec<String>), VivariumError> {
    let mut active = 0;
    let mut busy = Vec::new();
    for state in [RUN_STARTING, RUN_LIVE, RUN_RESTARTING] {
        for run in store.runs_in_state(state)? {
            if run.kind == FlowKind::Repair.name() {
                busy.push(target_of(mail, &run.need)?);
            } else {
                active += 1;
            }
        }
    }
    Ok((active, busy))
}

/// The needs with an open record, an open need state, no run in flight, and
/// no `investigate` mark, with their rank inputs.
fn candidates<M: FlowMail>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &M,
) -> Result<Vec<Candidate>, VivariumError> {
    let priorities: BTreeMap<String, i64> = store.need_priorities()?.into_iter().collect();
    let mut found = Vec::new();
    for (need, date) in mail.open_record_needs()? {
        if mail.need_state(&need)? != NeedStatus::Open || has_run_in_flight(store, &need)? {
            continue;
        }
        let marks = mail.need_marks(&need)?;
        if marks.iter().any(|m| m == MARK_INVESTIGATE) {
            continue;
        }
        let body = MailReader::open_record(mail, &need)?.map(|(_, body)| body);
        let (flow_kind, _) = kind::of(body.as_deref(), &marks);
        if let Err(error) = load(flow_kind) {
            raise_once(ctx, FLOW_INVALID, &need, &error.to_string());
            continue;
        }
        let repair = flow_kind == FlowKind::Repair;
        let target = match body.as_deref() {
            Some(text) if repair => target_line(text),
            _ => String::new(),
        };
        found.push(Candidate {
            repair,
            target,
            priority: priorities.get(&need).copied().unwrap_or(0),
            age: age_of(&date),
            need,
        });
    }
    Ok(found)
}

fn has_run_in_flight(store: &PumpStore, need: &str) -> Result<bool, VivariumError> {
    Ok(store.runs_of(need)?.iter().any(|run| {
        matches!(
            run.state.as_str(),
            RUN_STARTING | RUN_LIVE | RUN_FINISHING | RUN_RESTARTING
        )
    }))
}

fn target_of<M: FlowMail>(mail: &M, need: &str) -> Result<String, VivariumError> {
    Ok(MailReader::open_record(mail, need)?
        .map(|(_, body)| target_line(&body))
        .unwrap_or_default())
}

/// The `target:` line of a record body; empty when absent.
fn target_line(body: &str) -> String {
    body.lines()
        .find_map(|line| line.trim().strip_prefix("target:"))
        .map(|word| word.trim().to_owned())
        .unwrap_or_default()
}

/// Epoch seconds of an RFC 3339 or RFC 2822 date; `u64::MAX` when neither parses.
fn age_of(date: &str) -> u64 {
    match DateTime::parse_from_rfc3339(date).or_else(|_| DateTime::parse_from_rfc2822(date)) {
        Ok(at) => u64::try_from(at.timestamp()).unwrap_or(u64::MAX),
        Err(_) => u64::MAX,
    }
}

#[cfg(test)]
#[path = "intake_test.rs"]
mod tests;
