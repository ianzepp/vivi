//! Finding out that a job finished, collecting its result, and freeing the
//! slot: the engine behind `wait` and `reconcile`.
//!
//! A finished job leaves `/work/job/result` (key=value lines), `agent.out` (the
//! receipt) and the logs on the slot's persistent disk, so a result survives
//! the machine stopping. A running or lingering machine is read in place; a
//! stopped one is started, read, and stopped again. The result carries the
//! job id, so a leftover result from an earlier job is never mistaken for this
//! one's. Recording on the task comes before the slot is freed.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::provider::{MachineRecord, PoolProvider};
use super::record::OpenDispatch;
use super::slot::{self, META_CLAIMED, META_TASK, SlotStatus};
use crate::error::VivariumError;

/// Seconds past the budget before a silent job counts as timed out. The
/// slot's own hard limit is the budget plus the runner slack, so this only
/// trips when the slot itself is wedged.
pub const TIMEOUT_GRACE_SECS: u64 = 900;
/// A stopped slot younger than this is assumed to be still starting.
pub const START_GRACE_SECS: u64 = 60;
/// A claim mark with no dispatch behind it is stale after this long: more
/// than the time a dispatch spends between claiming a slot and recording.
pub const STALE_CLAIM_SECS: u64 = 300;

const SECTION_RESULT: &str = "@@vivi-pool result";
const SECTION_RECEIPT: &str = "@@vivi-pool receipt";
const SECTION_STDERR: &str = "@@vivi-pool stderr";
const SECTION_LOG: &str = "@@vivi-pool log";

/// Runs inside the slot. Prints nothing until the job's result exists, then
/// the result, the receipt, and the log tails, each under a marker line.
pub const COLLECT_SCRIPT: &str = r#"J=/work/job
[ -f "$J/result" ] || exit 0
echo '@@vivi-pool result'; cat "$J/result"
echo '@@vivi-pool receipt'; head -c 65536 "$J/agent.out" 2>/dev/null; echo
echo '@@vivi-pool stderr'; tail -c 4096 "$J/agent.err" 2>/dev/null; echo
echo '@@vivi-pool log'; tail -c 4096 "$J/run.log" 2>/dev/null; echo
"#;

/// How the engine paces its polling.
#[derive(Debug, Clone)]
pub struct Pacing {
    pub poll: Duration,
    /// How long to wait for a slot to accept commands after a start.
    pub ready_timeout: Duration,
    /// How long to wait for a stopped slot to read `stopped`.
    pub stop_timeout: Duration,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(3),
            ready_timeout: Duration::from_secs(90),
            stop_timeout: Duration::from_mins(1),
        }
    }
}

/// The `result` file: key=value lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JobResult {
    pub fields: BTreeMap<String, String>,
}

impl JobResult {
    /// Parse key=value lines; lines without `=` are ignored.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let fields = text
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        Self { fields }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> &str {
        self.fields.get(key).map_or("", String::as_str)
    }

    /// The push outcome word (`pushed`, `push_refused`, `no_changes`, ...).
    #[must_use]
    pub fn push(&self) -> &str {
        self.get("push")
    }

    /// `pushed <branch> <sha>`, or the bare outcome word.
    #[must_use]
    pub fn push_line(&self) -> String {
        match (self.push(), self.get("sha")) {
            ("pushed", sha) if !sha.is_empty() => {
                format!(
                    "pushed {} {}",
                    self.get("branch"),
                    sha.get(..10).unwrap_or(sha)
                )
            }
            (word, _) => word.to_string(),
        }
    }

    /// Two plain lines for the receipt.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "push: {}\nagent exit {} in {}s, job {}s, started from {}",
            self.push_line(),
            self.get("agent_rc"),
            self.get("agent_s"),
            self.get("job_s"),
            self.get("start")
                .get(..10)
                .unwrap_or_else(|| self.get("start")),
        )
    }
}

/// What a finished job left on the slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collected {
    pub result: JobResult,
    pub receipt: String,
    pub stderr: String,
    pub log: String,
}

/// Parse the collect script's output. `None` when it is empty or lacks a
/// result.
#[must_use]
pub fn parse_collected(output: &str) -> Option<Collected> {
    let mut sections: BTreeMap<&str, String> = BTreeMap::new();
    let mut current: Option<&str> = None;
    for line in output.lines() {
        let marker = [SECTION_RESULT, SECTION_RECEIPT, SECTION_STDERR, SECTION_LOG]
            .into_iter()
            .find(|m| line == *m);
        if let Some(marker) = marker {
            current = Some(marker);
            sections.entry(marker).or_default();
        } else if let Some(section) = current {
            let body = sections.entry(section).or_default();
            body.push_str(line);
            body.push('\n');
        }
    }
    let result = JobResult::parse(sections.get(SECTION_RESULT)?);
    if result.get("job").is_empty() {
        return None;
    }
    let take = |key: &str| sections.get(key).cloned().unwrap_or_default();
    Some(Collected {
        result,
        receipt: take(SECTION_RECEIPT),
        stderr: take(SECTION_STDERR),
        log: take(SECTION_LOG),
    })
}

/// How a dispatch ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Finished(Collected),
    /// No result within the budget plus grace.
    Timeout,
    /// The slot stopped (or vanished) without writing a result.
    Lost,
}

impl Outcome {
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Self::Finished(_) => "finished",
            Self::Timeout => "timeout",
            Self::Lost => "lost",
        }
    }
}

/// Read this job's result from a running slot. `Ok(None)` means the slot
/// answered and holds no result of this job (none yet, or an earlier job's).
///
/// # Errors
/// Returns the provider error when the slot could not be asked: that is not
/// the same as "still running" and must not be treated as it.
fn try_collect(
    provider: &dyn PoolProvider,
    machine: &str,
    job: &str,
) -> Result<Option<Collected>, VivariumError> {
    let output = provider.exec(machine, COLLECT_SCRIPT, 20)?;
    Ok(parse_collected(&output.stdout).filter(|c| c.result.get("job") == job))
}

/// Where one open dispatch stands. `None` means still running.
///
/// # Errors
/// Returns an error if the provider cannot report the machine, cannot start a
/// stopped one, or cannot read a running one before the dispatch is overdue.
pub fn poll(
    provider: &dyn PoolProvider,
    dispatch: &OpenDispatch,
    now: u64,
    pacing: &Pacing,
) -> Result<Option<Outcome>, VivariumError> {
    let record = &dispatch.record;
    let machine = provider.machine(&record.machine)?;
    let age = now.saturating_sub(record.at);
    let overdue = age > record.budget + TIMEOUT_GRACE_SECS;
    match machine.state.as_str() {
        "stopped" => collect_stopped(provider, dispatch, age, overdue, pacing),
        "destroyed" | "failed" => Ok(Some(Outcome::Lost)),
        "started" => collect_running(provider, dispatch, overdue),
        // starting, stopping, replacing: look again next round.
        _ => Ok(overdue.then_some(Outcome::Timeout)),
    }
}

fn collect_running(
    provider: &dyn PoolProvider,
    dispatch: &OpenDispatch,
    overdue: bool,
) -> Result<Option<Outcome>, VivariumError> {
    let record = &dispatch.record;
    match try_collect(provider, &record.machine, &record.job) {
        Ok(Some(done)) => Ok(Some(Outcome::Finished(done))),
        Ok(None) | Err(_) if overdue => Ok(Some(Outcome::Timeout)),
        Ok(None) => Ok(None),
        Err(e) => Err(e),
    }
}

fn collect_stopped(
    provider: &dyn PoolProvider,
    dispatch: &OpenDispatch,
    age: u64,
    overdue: bool,
    pacing: &Pacing,
) -> Result<Option<Outcome>, VivariumError> {
    let record = &dispatch.record;
    if age < START_GRACE_SECS && !overdue {
        return Ok(None);
    }
    provider.start(&record.machine)?;
    let deadline = Instant::now() + pacing.ready_timeout;
    loop {
        let unanswered = match try_collect(provider, &record.machine, &record.job) {
            Ok(Some(done)) => return Ok(Some(Outcome::Finished(done))),
            Ok(None) => None,
            Err(e) => Some(e),
        };
        if Instant::now() >= deadline {
            return match (overdue, unanswered) {
                (true, _) => Ok(Some(Outcome::Timeout)),
                // The slot never answered: that says nothing about the job.
                (false, Some(e)) => Err(e),
                (false, None) => Ok(Some(Outcome::Lost)),
            };
        }
        std::thread::sleep(pacing.poll);
    }
}

/// Free a slot held by `task`: stop it if it runs and confirm it stopped, then
/// drop the claim marks. Marks go last, so a slot never looks free while it
/// still runs, and a slot that would not stop keeps its marks (a later
/// `reconcile` retries). A slot that `task` no longer holds is left alone
/// unless it runs with no claim at all (a collector started it after the
/// holder freed it): that one is stopped.
///
/// # Errors
/// Returns an error if the machine cannot be read, will not stop, or cannot
/// be unmarked.
pub fn release(
    provider: &dyn PoolProvider,
    machine_id: &str,
    task: &str,
    pacing: &Pacing,
) -> Result<(), VivariumError> {
    let machine = provider.machine(machine_id)?;
    if machine.metadata.get(META_TASK).map(String::as_str) != Some(task) {
        return stop_unclaimed(provider, &machine, pacing);
    }
    stop_confirmed(provider, machine_id, pacing)?;
    provider.clear_metadata(machine_id, META_TASK)?;
    provider.clear_metadata(machine_id, META_CLAIMED)
}

/// Stop a seeded machine that runs with no task holding it.
///
/// # Errors
/// Returns an error if the machine will not stop.
pub fn stop_unclaimed(
    provider: &dyn PoolProvider,
    machine: &MachineRecord,
    pacing: &Pacing,
) -> Result<(), VivariumError> {
    let runs_free =
        slot::slot_state(machine) == "busy" && !machine.metadata.contains_key(META_TASK);
    if runs_free {
        stop_confirmed(provider, &machine.id, pacing)?;
    }
    Ok(())
}

/// Stop a machine and read it back until it says `stopped`; ask again when it
/// still runs.
fn stop_confirmed(
    provider: &dyn PoolProvider,
    id: &str,
    pacing: &Pacing,
) -> Result<(), VivariumError> {
    let deadline = Instant::now() + pacing.stop_timeout;
    let mut asked = false;
    let mut state = provider.machine(id)?.state;
    loop {
        match state.as_str() {
            "stopped" | "destroyed" => return Ok(()),
            "stopping" if asked => {}
            _ => match provider.stop(id) {
                Err(e) if !asked => return Err(e),
                _ => asked = true,
            },
        }
        std::thread::sleep(pacing.poll);
        state = provider.machine(id)?.state;
        if matches!(state.as_str(), "stopped" | "destroyed") {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(VivariumError::Message(format!(
                "machine {id} did not stop (it is {state})"
            )));
        }
    }
}

/// Claimed slots with no open dispatch behind them, claimed long enough ago
/// that no dispatch is still being set up.
#[must_use]
pub fn stale_claims<'a>(
    slots: &'a [SlotStatus],
    open: &[OpenDispatch],
    now: u64,
) -> Vec<&'a SlotStatus> {
    slots
        .iter()
        .filter(|s| s.task.is_some())
        .filter(|s| !open.iter().any(|d| d.record.machine == s.id))
        .filter(|s| {
            s.claimed_at
                .is_none_or(|at| now.saturating_sub(at) > STALE_CLAIM_SECS)
        })
        .collect()
}

/// Running slots that no task holds and no dispatch explains: a billing leak.
/// A dispatch marks its slot before it starts it, so an unmarked running slot
/// is always left over from a collector or a failed start.
#[must_use]
pub fn orphan_runners<'a>(slots: &'a [SlotStatus], open: &[OpenDispatch]) -> Vec<&'a SlotStatus> {
    slots
        .iter()
        .filter(|s| s.state == "busy" && s.task.is_none())
        .filter(|s| !open.iter().any(|d| d.record.machine == s.id))
        .collect()
}

/// Remembers which poll errors were already shown, so a long `wait` shows
/// each new problem once and says when it clears. An error and "still
/// running" must never look alike.
#[derive(Debug, Default)]
pub struct ErrorWatch {
    shown: BTreeMap<String, String>,
}

impl ErrorWatch {
    /// The notices to print for this round: each new or changed error, and
    /// each task whose polls work again.
    pub fn observe(&mut self, lines: &[RoundLine]) -> Vec<String> {
        let mut notices = Vec::new();
        for line in lines {
            if line.state == "error" {
                if self.shown.get(&line.task) != Some(&line.detail) {
                    notices.push(format!(
                        "warning: cannot poll {} on {}: {}",
                        line.task, line.slot, line.detail
                    ));
                    self.shown.insert(line.task.clone(), line.detail.clone());
                }
            } else if self.shown.remove(&line.task).is_some() {
                notices.push(format!("recovered: polling {} works again", line.task));
            }
        }
        notices
    }
}

/// One line of a round's report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundLine {
    pub task: String,
    pub slot: String,
    /// `running`, `finished`, `timeout`, `lost`, or `error`.
    pub state: &'static str,
    pub detail: String,
}

impl RoundLine {
    /// True when the dispatch is settled (recorded and freed).
    #[must_use]
    pub fn settled(&self) -> bool {
        matches!(self.state, "finished" | "timeout" | "lost")
    }
}

/// Poll every open dispatch once. A settled one is recorded through `record`
/// first, and only then is its slot freed. `record` returns `false` when the
/// dispatch was already closed elsewhere: then nothing is freed and no line is
/// reported. One dispatch failing to poll does not stop the others; a failing
/// `record` stops the round with the slot left as it was, so the next round
/// retries.
///
/// # Errors
/// Returns the error `record` returns.
pub fn run_round(
    provider: &dyn PoolProvider,
    open: &[OpenDispatch],
    now: u64,
    pacing: &Pacing,
    record: &mut dyn FnMut(&OpenDispatch, &Outcome) -> Result<bool, VivariumError>,
) -> Result<Vec<RoundLine>, VivariumError> {
    let mut lines = Vec::new();
    for dispatch in open {
        let line = |state, detail: String| RoundLine {
            task: dispatch.task.clone(),
            slot: dispatch.record.slot.clone(),
            state,
            detail,
        };
        match poll(provider, dispatch, now, pacing) {
            Err(e) => lines.push(line("error", e.to_string())),
            Ok(None) => {
                let age = now.saturating_sub(dispatch.record.at);
                let detail = format!("{age}s of {}s", dispatch.record.budget);
                lines.push(line("running", detail));
            }
            Ok(Some(outcome)) => {
                if !record(dispatch, &outcome)? {
                    // Closed elsewhere. This poll may have started the slot
                    // after its holder freed it: never leave it running. A
                    // slot still marked is the holder's to free.
                    let stopped = provider
                        .machine(&dispatch.record.machine)
                        .and_then(|m| stop_unclaimed(provider, &m, pacing));
                    if let Err(e) = stopped {
                        eprintln!("warning: could not stop slot {}: {e}", dispatch.record.slot);
                    }
                    continue;
                }
                let mut detail = match &outcome {
                    Outcome::Finished(done) => done.result.push_line(),
                    Outcome::Timeout => "no result; slot stopped".to_string(),
                    Outcome::Lost => "slot ended without a result".to_string(),
                };
                // The result is recorded; a slot that would not free is
                // retried by `reconcile`, and must not hide this result.
                if let Err(e) = release(provider, &dispatch.record.machine, &dispatch.task, pacing)
                {
                    detail = format!("{detail}; slot not freed yet ({e})");
                }
                lines.push(line(outcome.word(), detail));
            }
        }
    }
    Ok(lines)
}

#[cfg(test)]
#[path = "collect_test.rs"]
mod tests;
