//! Shared build prerequisite assessment: the one place intake and restart
//! both ask whether a build may be admitted.
//!
//! The admitted rule: permit admission only when every prerequisite is
//! `Landed`. Ordinary `Open` prerequisites are silent; `Held` and
//! `ClosedUnlanded` each raise their identifying diagnostic, and every
//! non-landed prerequisite is kept in the typed projection queued as the
//! need's build wait.

use crate::pump::flow::def::FlowKind;
use crate::pump::flow::kind;
use crate::pump::flow::mail::MailReader;
use crate::pump::flow::milestone::prereq::{Prereq, PrereqState, prerequisites};
use crate::pump::host::Ctx;
use crate::pump::store::PumpStore;
use crate::pump::store::build_waits::{BuildPrerequisite, BuildWait, PrerequisiteState};
use vivi_error::VivariumError;

/// Exception kind: a prerequisite is open but held by an operator-only state.
pub(crate) const PREREQ_HELD: &str = "flow-build-prereq-held";

/// Exception kind: a prerequisite closed without landing.
pub(crate) const PREREQ_UNLANDED: &str = "flow-build-prereq-unlanded";

/// Exception kind: the dependent's prerequisites could not be read.
pub(crate) const PREREQ_UNREADABLE: &str = "flow-build-prereq-unreadable";

/// Whether a need may be admitted, and why not when it may not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Assessment {
    /// The effective kind of the need's open record (or marks).
    pub kind: FlowKind,
    /// True when admission is permitted.
    pub eligible: bool,
    /// The non-landed prerequisites, in backlog order.
    pub blockers: Vec<BuildPrerequisite>,
}

/// Assess `need` for admission. Only builds are gated; a non-build need is
/// always eligible and its wait is removed. A build is eligible exactly when
/// every prerequisite is `Landed`.
///
/// The wait is updated in the same call: an eligible or non-build assessment
/// removes it, a waiting one queues the typed projection.
///
/// # Errors
/// Returns [`VivariumError`] when the open record, marks, prerequisites or the
/// wait cannot be read or written. A prerequisites read error is raised as
/// [`PREREQ_UNREADABLE`] and leaves the previous wait alone.
pub(crate) fn assess<M: MailReader>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &M,
    need: &str,
) -> Result<Assessment, VivariumError> {
    let record = mail.open_record(need)?;
    let marks = mail.need_marks(need)?;
    let kind = kind::of(record.as_ref().map(|(_, body)| body.as_str()), &marks);
    let Some((record, _)) = record else {
        store.remove_build_wait(need)?;
        return Ok(Assessment {
            kind,
            eligible: true,
            blockers: Vec::new(),
        });
    };
    let blockers = read_blockers(ctx, store, mail, need, &record)?;
    if kind != FlowKind::Build {
        store.remove_build_wait(need)?;
        return Ok(Assessment {
            kind,
            eligible: true,
            blockers: Vec::new(),
        });
    }
    for blocker in &blockers {
        diagnose(ctx, store, need, &record, blocker)?;
    }
    let eligible = blockers.is_empty();
    if eligible {
        store.remove_build_wait(need)?;
    } else {
        let wait = BuildWait {
            need: need.to_string(),
            record: record.clone(),
            prerequisites: blockers.clone(),
            observed_at: ctx.now(),
        };
        store.upsert_build_wait(&wait)?;
    }
    Ok(Assessment {
        kind,
        eligible,
        blockers,
    })
}

/// Every non-landed prerequisite of `need`, in the typed projection.
fn read_blockers<M: MailReader>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &M,
    need: &str,
    record: &str,
) -> Result<Vec<BuildPrerequisite>, VivariumError> {
    let found = prerequisites(store, mail, need).map_err(|error| {
        unreadable(ctx, store, need, record, &error);
        error
    })?;
    Ok(found.into_iter().filter_map(blocker).collect())
}

/// The typed projection of one non-landed prerequisite; `None` when landed.
fn blocker(prereq: Prereq) -> Option<BuildPrerequisite> {
    let handle = prereq.handle;
    match prereq.state {
        PrereqState::Landed => None,
        PrereqState::Open => Some(BuildPrerequisite {
            handle,
            state: PrerequisiteState::Open,
            detail: None,
            outcome: None,
        }),
        PrereqState::Held(detail) => Some(BuildPrerequisite {
            handle,
            state: PrerequisiteState::Held,
            detail: Some(detail),
            outcome: None,
        }),
        PrereqState::ClosedUnlanded(outcome) => Some(BuildPrerequisite {
            handle,
            state: PrerequisiteState::ClosedUnlanded,
            detail: None,
            outcome: Some(outcome),
        }),
    }
}

/// Raise the diagnostic of one exceptional blocker, durably deduplicated by
/// open record, dependent, prerequisite and diagnostic kind.
fn diagnose(
    ctx: &Ctx,
    store: &PumpStore,
    need: &str,
    record: &str,
    blocker: &BuildPrerequisite,
) -> Result<(), VivariumError> {
    let (kind, text) = match blocker.state {
        PrerequisiteState::Open => return Ok(()),
        PrerequisiteState::Held => {
            let detail = blocker.detail.as_deref().unwrap_or("open");
            let text = format!(
                "build {need} waits for {}; expected it to land; actual: open and held at {detail}",
                blocker.handle
            );
            (PREREQ_HELD, text)
        }
        PrerequisiteState::ClosedUnlanded => {
            let outcome = blocker.outcome.as_deref().unwrap_or("closed");
            let text = format!(
                "build {need} expects {} landed on main; actual: closed, outcome `{outcome}`",
                blocker.handle
            );
            (PREREQ_UNLANDED, text)
        }
    };
    let unit = format!("{record}/{need}/{}", blocker.handle);
    if store.has_exception(kind, &unit)? {
        return Ok(());
    }
    ctx.exception(kind, &unit, &text);
    Ok(())
}

/// Raise [`PREREQ_UNREADABLE`] once per open record and dependent. The
/// previous wait is left untouched.
fn unreadable(ctx: &Ctx, store: &PumpStore, need: &str, record: &str, error: &VivariumError) {
    let unit = format!("{record}/{need}");
    if store
        .has_exception(PREREQ_UNREADABLE, &unit)
        .unwrap_or(false)
    {
        return;
    }
    let text =
        format!("build {need}: expected its prerequisites from the backlog graph; actual: {error}");
    ctx.exception(PREREQ_UNREADABLE, &unit, &text);
}

#[cfg(test)]
#[path = "build_prereq_test.rs"]
mod tests;
