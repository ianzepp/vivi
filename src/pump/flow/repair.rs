//! Repair flow helpers: attempts, branch names, the repair record lines, attempt facts.

use crate::error::VivariumError;
use crate::letter::read::record_of;
use crate::letter::records::NoteWord;
use crate::pump::flow::slice_dispatch::SliceMail;
use crate::pump::store::PumpStore;

/// The per-attempt time cap in seconds: 30 minutes.
pub(crate) const ATTEMPT_SECS: u64 = 1800;

/// The attempt (1 or 2) a repair node belongs to: `fix<N>`, `rung<N>` and
/// `merge<N>` give N; any other node gives `None`.
pub(crate) fn attempt_of(node: &str) -> Option<u32> {
    let digits = ["fix", "rung", "merge"]
        .iter()
        .find_map(|prefix| node.strip_prefix(prefix))?;
    match digits {
        "1" => Some(1),
        "2" => Some(2),
        _ => None,
    }
}

/// The fix node of `attempt`: `fix<N>`.
pub(crate) fn fix_node(attempt: u32) -> String {
    format!("fix{attempt}")
}

/// The rung node of `attempt`: `rung<N>`.
pub(crate) fn rung_node(attempt: u32) -> String {
    format!("rung{attempt}")
}

/// The branch an attempt publishes to: `vivi/repair/<need>-r<run>-a<N>`, the
/// one segment a repairer may publish. The run number keeps a restarted run
/// off the refs an earlier run already pushed.
pub(crate) fn branch(need: &str, run: u32, attempt: u32) -> String {
    format!("vivi/repair/{need}-r{run}-a{attempt}")
}

/// What the open pump record of a repair need says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepairRecord {
    /// The target whose main is red.
    pub target: String,
    /// The main tip that was tested, when the record names one.
    pub tested: Option<String>,
    /// The failing output tail: the record prose after the front matter.
    pub tail: String,
}

/// Read a repair need's open record body.
///
/// # Errors
/// Returns [`VivariumError::Parse`] when the front matter does not parse or
/// names no `target`, or when the body has no front matter.
pub(crate) fn record(body: &str) -> Result<RepairRecord, VivariumError> {
    match record_of("", body) {
        Ok(envelope) => {
            let target = envelope.front.target.ok_or_else(|| {
                VivariumError::Parse(
                    "repair record: expected `target` in the record front matter".to_owned(),
                )
            })?;
            Ok(RepairRecord {
                target,
                tested: envelope.front.tested,
                tail: envelope.body.trim().to_owned(),
            })
        }
        Err(error) => Err(VivariumError::Parse(format!("repair record: {error}"))),
    }
}

/// What a fix attempt published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttemptFacts {
    /// The attempt branch.
    pub branch: String,
    /// The commit on it.
    pub commit: String,
    /// The main tip it was cut from.
    pub base: String,
}

/// The facts as `branch=<b> commit=<sha> base=<sha>`. A writer posts them as a
/// `repair-attempt` pump note whose prose is this text.
pub(crate) fn facts_text(facts: &AttemptFacts) -> String {
    format!(
        "branch={} commit={} base={}",
        facts.branch, facts.commit, facts.base
    )
}

/// Read the first prose line of a `repair-attempt` note; `None` when it is not
/// a complete facts line.
pub(crate) fn parse_facts(prose: &str) -> Option<AttemptFacts> {
    let payload = prose.trim().lines().next()?;
    let (mut branch, mut commit, mut base) = (None, None, None);
    for word in payload.split_whitespace() {
        match word.split_once('=') {
            Some(("branch", value)) => branch = Some(value.to_owned()),
            Some(("commit", value)) => commit = Some(value.to_owned()),
            Some(("base", value)) => base = Some(value.to_owned()),
            _ => {}
        }
    }
    Some(AttemptFacts {
        branch: branch?,
        commit: commit?,
        base: base?,
    })
}

/// The newest facts of `attempt` in a run: the newest `fix<N>` step row, then
/// the newest parseable `repair-attempt` note on its task that `pump` (the
/// pump identity) sent. Notes from anyone else are ignored.
///
/// # Errors
/// Returns [`VivariumError`] when the steps or the replies cannot be read.
pub(crate) fn latest_facts(
    store: &PumpStore,
    mail: &dyn SliceMail,
    pump: &str,
    need: &str,
    run: u32,
    attempt: u32,
) -> Result<Option<AttemptFacts>, VivariumError> {
    let node = fix_node(attempt);
    let steps = store.steps_of(need, run)?;
    let Some(step) = steps.iter().rev().find(|step| step.node == node) else {
        return Ok(None);
    };
    let notes = mail.notes_from(&step.task, NoteWord::RepairAttempt, pump)?;
    Ok(notes
        .iter()
        .rev()
        .find_map(|(_, note)| parse_facts(&note.prose)))
}

/// The `tested=<commit>` word that opens a `repair-rung` note's prose; `None`
/// when the prose has no such word.
pub(crate) fn parse_tested(prose: &str) -> Option<String> {
    let payload = prose.trim().lines().next()?;
    let word = payload.split_whitespace().next()?;
    let commit = word.strip_prefix("tested=")?.trim_end_matches(';');
    (!commit.is_empty()).then(|| commit.to_owned())
}

/// The commit the rung step of `attempt` tested: the newest `rung<N>` step row
/// of the run, then the newest `repair-rung` note on its task that `pump`
/// sent and that names a `tested=` commit. `None` when there is no such note.
///
/// # Errors
/// Returns [`VivariumError`] when the steps or the replies cannot be read.
pub(crate) fn tested_commit(
    store: &PumpStore,
    mail: &dyn SliceMail,
    pump: &str,
    need: &str,
    run: u32,
    attempt: u32,
) -> Result<Option<String>, VivariumError> {
    let node = rung_node(attempt);
    let steps = store.steps_of(need, run)?;
    let Some(step) = steps.iter().rev().find(|step| step.node == node) else {
        return Ok(None);
    };
    let notes = mail.notes_from(&step.task, NoteWord::RepairRung, pump)?;
    Ok(notes
        .iter()
        .rev()
        .find_map(|(_, note)| parse_tested(&note.prose)))
}

#[cfg(test)]
#[path = "repair_test.rs"]
mod tests;

#[cfg(test)]
#[path = "repair_flow_test.rs"]
mod flow_tests;
