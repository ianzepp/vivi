//! Dispatch of slice tasks to hands.
//!
//! A slice is already a mailspace task, so its hand job runs on that task
//! (`JobSpec.task`): nothing is sent, the pool takes the task as it is. The
//! job body carries the hand rules, the slice's current write scope, the hand
//! envelope contract and the deviations of slices that already landed.
//!
//! Facts come from machines, judgment from models. [`classify`] decides the
//! outcome of a finished hand job from the runner's `agent_rc` and `push`
//! result; the hand's envelope only supplies judgment (a scope request) and
//! reports. A disagreement raises `flow-envelope-disagrees` and the machine
//! wins.
//!
//! Attempts: a failed attempt re-dispatches the same task with the next
//! attempt number ([`retry`]), up to [`MAX_ATTEMPTS`]. A blocked-scope answer
//! and an envelope correction use no attempt ([`redispatch_scope`]).

use crate::error::VivariumError;
use crate::mailspace::{MailspaceThreadMessage, marked_payload, order_marked};
use crate::pump::attempt_record::{Ending, hand_message, post_on_task};
use crate::pump::brief::hand_rules;
use crate::pump::envelope::EnvelopeError;
use crate::pump::envelope::correct::correction_n;
use crate::pump::envelope::parse;
use crate::pump::envelope::roles::{CheckResult, Hand, HandAnswer};
use crate::pump::flow::collect::{ENVELOPE_DISAGREES, ENVELOPE_FAILED, MAX_CORRECTIONS};
use crate::pump::flow::fanout::{VERDICT_MERGED, is_slice_row, mark_exhausted, target_of};
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailspacePort, TaskStatus};
use crate::pump::flow::need_branch;
use crate::pump::flow::scope::{BlockedScope, ScopeState};
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::need::unit_branch;
use crate::pump::store::PumpStore;

/// Job kind of a slice hand job.
pub const KIND: &str = "hand";

/// Exception kind: a built envelope reported a failing `[[check]]`.
pub const HAND_REPORTED_FAILURE: &str = "flow-hand-reported-failure";

/// A slice gets this many attempts; the failure of the last one exhausts it.
pub const MAX_ATTEMPTS: u32 = 2;

/// First line of a deviations reply on a slice task (posted by the tick).
pub const DEVIATIONS_MARKER: &str = "pump-deviations:";

/// Most bytes of landed-slice deviations a brief carries.
const DEVIATIONS_CAP: usize = 4 * 1024;

/// Line that closes the deviations when the oldest were cut.
const DEVIATIONS_CUT: &str = "[older deviations cut]";

/// The hand envelope contract, opening text. Appended to every slice brief,
/// whatever `[pump] hand_rules` says: it overrides any older rule about
/// `VERDICT: no-defect`, `OUT-OF-SCOPE:` lines or minimal outside-scope edits.
const CONTRACT_HEAD: &str = "HAND ENVELOPE CONTRACT (overrides any earlier rule about `VERDICT: no-defect`, `OUT-OF-SCOPE:` lines, or changing files outside WRITE SCOPE)

Your final message opens with TOML front matter between two lines containing only `+++`, then your report as prose. Example:
";

/// A built (or failed) hand envelope, as the contract shows it.
const EXAMPLE_BUILT: &str = "+++
answer = \"built\"
change = [\"src/a.rs\", \"src/a_test.rs\"]
deviations = ['changed a signature a later slice must know']
remaining = []

[[check]]
cmd = \"cargo test --lib a --quiet\"
result = \"pass\"
seconds = 40

[[follow_up]]
paths = [\"src/b.rs\"]
need = 'work outside this slice that someone must do'
+++
What I did, what I verified, what is left.";

/// A blocked-scope hand envelope, as the contract shows it.
const EXAMPLE_BLOCKED: &str = "+++
answer = \"blocked-scope\"
scope_paths = [\"src/other.rs\"]
scope_reason = 'one line: why the work needs this path'
scope_design = false
+++
Nothing was changed.";

/// The contract's rules, after the examples.
const CONTRACT_RULES: &str = "Rules:
- `answer` is `built`, `blocked-scope` or `failed`. Unknown keys are refused. Write free text as literal strings '...' (or '''...''' for several lines). Lists default to empty.
- If the work needs a path outside WRITE SCOPE, change NOTHING and answer `blocked-scope` with `scope_paths` (every extra path), `scope_reason` and, only when the need is a design decision rather than a missing path, `scope_design = true`. This is not a failure.
- If you find nothing to change, change nothing and answer `built`; the runner sees that no files changed.
- `failed` means you could not finish; say what is left in `remaining`.
- Put surprises a later slice must know in `deviations`, and work outside this slice in `[[follow_up]]` (`paths`, `need`).
- `change` and `[[check]]` are reports only. The pump decides the outcome from the runner's exit code and push result, never from your envelope, so do not claim SHAs, refs, merges or exit codes.";

/// What a finished hand job amounts to.
#[derive(Debug, Clone, PartialEq)]
pub enum AttemptResult {
    /// The hand delivered a branch (the runner pushed or committed it).
    Built { envelope: Hand, branch: String },
    /// The hand changed nothing (the runner saw `no_changes`).
    NoDefect,
    /// The hand needs paths outside its write scope; no attempt is used.
    BlockedScope(BlockedScope),
    /// The attempt failed: the tick retries or exhausts the slice.
    Failed,
    /// The envelope did not parse: run a correction job (no attempt is used).
    Correct { error: EnvelopeError },
}

/// What [`retry`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Retried {
    /// The next attempt was queued under this job id.
    Dispatched(String),
    /// The attempt cap was reached: the slice is marked exhausted.
    Exhausted,
}

/// The mailspace reads a hand brief needs beyond a task's state.
pub trait SliceMail: MailReader {
    /// The body of `task`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the task cannot be read.
    fn task_body(&self, task: &str) -> Result<String, VivariumError>;

    /// Replies on `task` whose text starts with `marker`, oldest first, each
    /// as `(sortable date, trimmed text)`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the thread cannot be read.
    fn marked_replies(
        &self,
        task: &str,
        marker: &str,
    ) -> Result<Vec<(String, String)>, VivariumError>;

    /// Like [`SliceMail::marked_replies`], but only the replies sent by the
    /// identity `sender` (a role name or alias). A reply whose sender does not
    /// resolve to `sender` is left out.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the thread cannot be read.
    fn marked_replies_from(
        &self,
        task: &str,
        marker: &str,
        sender: &str,
    ) -> Result<Vec<(String, String)>, VivariumError>;

    /// The handle of this mailspace's own copy of the want `id` names (the
    /// `explores` or `serves` value a need stores), or `None` when no copy of
    /// that want is here.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the mailspace cannot be read.
    fn resolve_want(&self, id: &str) -> Result<Option<String>, VivariumError>;
}

impl SliceMail for MailspacePort {
    fn resolve_want(&self, id: &str) -> Result<Option<String>, VivariumError> {
        self.mailspace.resolve_want(id)
    }

    fn task_body(&self, task: &str) -> Result<String, VivariumError> {
        let (id, thread) = self.thread_of(task)?;
        thread
            .into_iter()
            .find(|message| message.message_id == id)
            .map(|message| message.body)
            .ok_or_else(|| VivariumError::Message(format!("task {task} has no body in its thread")))
    }

    fn marked_replies(
        &self,
        task: &str,
        marker: &str,
    ) -> Result<Vec<(String, String)>, VivariumError> {
        self.marked_where(task, marker, |_| true)
    }

    fn marked_replies_from(
        &self,
        task: &str,
        marker: &str,
        sender: &str,
    ) -> Result<Vec<(String, String)>, VivariumError> {
        let wanted = self.mailspace.resolve_identity(sender);
        self.marked_where(task, marker, |message| match &wanted {
            Ok(wanted) => self.sent_by(message, wanted),
            Err(_) => false,
        })
    }
}

/// The bare address inside a stored `From` value (`Name <a@b>` or `a@b`).
fn bare_address(from: &str) -> &str {
    match (from.find('<'), from.rfind('>')) {
        (Some(open), Some(close)) if open < close => from[open + 1..close].trim(),
        _ => from.trim(),
    }
}

impl MailspacePort {
    /// Whether `message` came from the identity `wanted` (already resolved).
    /// A sender that resolves to no known role is not a match.
    fn sent_by(&self, message: &MailspaceThreadMessage, wanted: &str) -> bool {
        match self.mailspace.resolve_identity(bare_address(&message.from)) {
            Ok(from) => from == wanted,
            Err(_) => false,
        }
    }

    /// The marked replies on `task` that `keep` accepts, oldest first, each as
    /// `(sortable date, trimmed text)`.
    fn marked_where(
        &self,
        task: &str,
        marker: &str,
        keep: impl Fn(&MailspaceThreadMessage) -> bool,
    ) -> Result<Vec<(String, String)>, VivariumError> {
        let (id, thread) = self.thread_of(task)?;
        let mut found: Vec<MailspaceThreadMessage> = thread
            .into_iter()
            .filter(|message| message.event.is_none() && message.message_id != id)
            .filter(|message| message.body.trim_start().starts_with(marker))
            .filter(|message| keep(message))
            .collect();
        order_marked(&mut found);
        Ok(found
            .into_iter()
            .map(|message| {
                let text = message.body.trim();
                let (first, more) = text.split_once('\n').unwrap_or((text, ""));
                let payload = marked_payload(first, marker).unwrap_or_default();
                let line = format!("{marker} {payload}").trim_end().to_owned();
                let text = if more.is_empty() {
                    line
                } else {
                    format!("{line}\n{more}")
                };
                (message.date, text)
            })
            .collect())
    }

    /// The task's resolved message id and its thread, oldest first.
    fn thread_of(
        &self,
        task: &str,
    ) -> Result<(String, Vec<MailspaceThreadMessage>), VivariumError> {
        let id = self.mailspace.storage()?.resolve_message_token(task)?;
        let thread = self.mailspace.thread(task, false, usize::MAX, usize::MAX)?;
        Ok((id, thread))
    }
}

/// Queue the hand job for attempt `attempt` of `slice` on its own task, and
/// record it as a job of the step. Returns the job id. Calling it again for
/// the same attempt queues nothing new.
///
/// # Errors
/// Returns [`VivariumError`] when there is no hand role, the task has no
/// target in its front matter or no recorded scope, or a mail, job or index call fails.
pub fn dispatch<S: JobStore, M: SliceMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &M,
    need: &str,
    slice: &StepRow,
    attempt: u32,
) -> Result<String, VivariumError> {
    enqueue_hand(ctx, st, store, mail, need, slice, attempt, None)
}

/// A failed attempt: post its record on the slice task, then queue the next
/// attempt on the same task. After [`MAX_ATTEMPTS`] the slice is marked
/// exhausted instead (the fanout then settles `failed`).
///
/// # Errors
/// Returns [`VivariumError`] when queueing the next attempt or marking the
/// slice fails.
pub fn retry<S: JobStore, M: SliceMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &M,
    need: &str,
    slice: &StepRow,
    failed: &PoolJob,
    attempt: u32,
) -> Result<Retried, VivariumError> {
    post_failed_attempt(ctx, &slice.task, failed);
    if attempt >= MAX_ATTEMPTS {
        mark_exhausted(store, &slice.task)?;
        return Ok(Retried::Exhausted);
    }
    dispatch(ctx, st, store, mail, need, slice, attempt + 1).map(Retried::Dispatched)
}

/// Re-dispatch the same task with the SAME attempt number after a
/// blocked-scope request was settled: the brief carries the current (possibly
/// widened) scope. The slice's scope state returns to clear. Returns the job
/// id (`slice-<task>-a<N>-s<M>`).
///
/// # Errors
/// Returns the errors of [`dispatch`], or a store error.
pub fn redispatch_scope<S: JobStore, M: SliceMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &M,
    need: &str,
    slice: &StepRow,
    attempt: u32,
) -> Result<String, VivariumError> {
    let mut round = 1;
    while store
        .step_job(&job_id(&slice.task, attempt, Some(round)))?
        .is_some()
    {
        round += 1;
    }
    let id = enqueue_hand(ctx, st, store, mail, need, slice, attempt, Some(round))?;
    store.set_scope_state(&slice.task, ScopeState::Clear)?;
    Ok(id)
}

/// `slice-<task>-a<N>`, or `slice-<task>-a<N>-s<M>` for a scope re-dispatch.
fn job_id(task: &str, attempt: u32, scope_round: Option<u32>) -> String {
    match scope_round {
        Some(round) => format!("slice-{task}-a{attempt}-s{round}"),
        None => format!("slice-{task}-a{attempt}"),
    }
}

fn enqueue_hand<S: JobStore, M: SliceMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &M,
    need: &str,
    slice: &StepRow,
    attempt: u32,
    scope_round: Option<u32>,
) -> Result<String, VivariumError> {
    let id = job_id(&slice.task, attempt, scope_round);
    let row = slice.job(&id, attempt);
    if !st.jobs().contains_key(&id) {
        let mut spec = hand_spec(ctx, store, mail, need, slice, attempt, &id, scope_round)?;
        row.stamp(need, &mut spec.payload);
        jobs::enqueue(st, spec)?;
    }
    if store.step_job(&id)?.is_none() {
        store.insert_step_job(need, &row)?;
    }
    store.set_step_state(&slice.task, StepState::Running, None)?;
    Ok(id)
}

fn hand_spec<M: SliceMail>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &M,
    need: &str,
    slice: &StepRow,
    attempt: u32,
    id: &str,
    scope_round: Option<u32>,
) -> Result<JobSpec, VivariumError> {
    let role = ctx.routing.hand(&ctx.cfg.to).ok_or_else(|| {
        VivariumError::Config(format!(
            "no hand role {:?} to run slice {}",
            ctx.cfg.to, slice.task
        ))
    })?;
    let task_body = mail.task_body(&slice.task)?;
    let target = target_of(&task_body).ok_or_else(|| {
        VivariumError::Parse(format!(
            "slice task {} names no target in its front matter",
            slice.task
        ))
    })?;
    let target = target.as_str();
    let scope = store.slice_scope(&slice.task)?.ok_or_else(|| {
        VivariumError::Parse(format!("slice task {} has no recorded scope", slice.task))
    })?;
    let body = compose(
        ctx,
        &scope,
        &landed_deviations(store, mail, need, &slice.task)?,
        &task_body,
    );
    let from = hand_start(ctx, store, need, slice.run, target)?;
    let target_ctx = ctx.for_target(target);
    let cfg = target_ctx.as_ref().map_or(&*ctx.cfg, |target| &*target.cfg);
    let mut branch = unit_branch(&slice.task, attempt);
    if let Some(round) = scope_round {
        branch = format!("{branch}-s{round}");
    }
    let mut spec = JobSpec::new(
        id,
        KIND,
        &role.name,
        format!("{} a{attempt} ({need})", slice.node),
        body,
    );
    spec.task = Some(slice.task.clone());
    spec.target = Some(target.to_owned());
    spec.from_ref = Some(from);
    spec.publish_ref = Some(format!("refs/heads/{branch}"));
    spec.branch = Some(branch);
    spec.budget = cfg.dispatch.budget;
    Ok(spec)
}

/// Where a hand starts: the need-branch tip. The first dispatch of a run
/// records the base. One need branch per need: a slice on another target is
/// refused.
fn hand_start(
    ctx: &Ctx,
    store: &PumpStore,
    need: &str,
    run: u32,
    target: &str,
) -> Result<String, VivariumError> {
    need_branch::record_base(ctx, store, need, run, target)?;
    if let Some(row) = store.need_branch(need)?
        && row.target != target
    {
        return Err(VivariumError::Config(format!(
            "need {need} has its need branch on target {}, but the slice targets {target}",
            row.target
        )));
    }
    need_branch::tip(ctx, store, need)?
        .ok_or_else(|| VivariumError::Other(format!("need {need} has no need-branch tip")))
}

/// The hand brief: rules, write scope, envelope contract, landed deviations,
/// then the slice task body.
fn compose(ctx: &Ctx, scope: &[String], deviations: &str, task_body: &str) -> String {
    let mut parts = vec![
        hand_rules(ctx),
        String::from("WRITE SCOPE:"),
        scope
            .iter()
            .map(|path| format!("- {path}"))
            .collect::<Vec<_>>()
            .join("\n"),
        String::new(),
        contract(),
    ];
    if !deviations.is_empty() {
        parts.push(String::new());
        parts.push(String::from("Deviations of landed slices (newest first):"));
        parts.push(deviations.to_owned());
    }
    parts.push(String::new());
    parts.push(task_body.trim().to_owned());
    parts.join("\n")
}

/// The hand envelope contract text.
#[must_use]
pub fn contract() -> String {
    format!(
        "{CONTRACT_HEAD}\n{EXAMPLE_BUILT}\n\nA scope request:\n\n{EXAMPLE_BLOCKED}\n\n{CONTRACT_RULES}"
    )
}

/// The `pump-deviations:` replies of this need's slice tasks that closed
/// `merged`, newest first, one entry each, cut at [`DEVIATIONS_CAP`] bytes
/// (the oldest go first). Empty when there are none. A slice never reads its
/// own deviations.
fn landed_deviations<M: SliceMail>(
    store: &PumpStore,
    mail: &M,
    need: &str,
    own_task: &str,
) -> Result<String, VivariumError> {
    let mut entries: Vec<(String, String)> = Vec::new();
    for run in store.runs_of(need)? {
        for row in store.steps_of(need, run.run)? {
            if !is_slice_row(&row) || row.task == own_task {
                continue;
            }
            let state = mail.task_state(&row.task)?;
            if state.status != TaskStatus::Done || state.verdict.as_deref() != Some(VERDICT_MERGED)
            {
                continue;
            }
            for (date, text) in mail.marked_replies(&row.task, DEVIATIONS_MARKER)? {
                let text = text.strip_prefix(DEVIATIONS_MARKER).unwrap_or(&text).trim();
                entries.push((date, format!("- {} ({}): {text}", row.node, row.task)));
            }
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.reverse();
    let joined = entries
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n");
    if joined.len() <= DEVIATIONS_CAP {
        return Ok(joined);
    }
    let mut end = DEVIATIONS_CAP;
    while !joined.is_char_boundary(end) {
        end -= 1;
    }
    Ok(format!("{}\n{DEVIATIONS_CUT}", &joined[..end]))
}

/// Post the record of a failed attempt on the slice task, so the next
/// attempt's brief carries it.
fn post_failed_attempt(ctx: &Ctx, task: &str, job: &PoolJob) {
    let (why, detail, receipt) = match &job.outcome {
        Some(outcome) => (
            match job.status {
                JobStatus::Timeout => String::from("job timed out"),
                JobStatus::Lost => String::from("job lost"),
                _ => String::from("hand failed"),
            },
            outcome.detail.clone(),
            Some(outcome.receipt.as_str()).filter(|text| !text.is_empty()),
        ),
        None => (
            String::from("job ended without a receipt"),
            String::new(),
            None,
        ),
    };
    post_on_task(
        ctx,
        task,
        job,
        &Ending::failed(&why, &detail, receipt),
        receipt.unwrap_or_default(),
    );
}

/// What a finished hand job amounts to, or `None` while it has not reached a
/// terminal pool state.
///
/// Machine facts first: job status, `agent_rc` and the runner's `push`
/// result. The envelope supplies judgment only.
#[must_use]
pub fn classify(ctx: &Ctx, job: &PoolJob) -> Option<AttemptResult> {
    classify_corrected(ctx, job, job)
}

/// Like [`classify`], for a hand whose envelope was rewritten by a correction
/// job: the machine facts come from `hand` (the original job), the envelope
/// and the correction count from `reply` (the correction job).
#[must_use]
pub fn classify_corrected(ctx: &Ctx, hand: &PoolJob, reply: &PoolJob) -> Option<AttemptResult> {
    if !hand.status.is_terminal() || !reply.status.is_terminal() {
        return None;
    }
    let task = hand.task.as_deref().unwrap_or(&hand.id);
    let (Some(facts), JobStatus::Finished) = (hand.outcome.as_ref(), hand.status) else {
        return Some(AttemptResult::Failed);
    };
    let agent_ok = facts.agent_success() == Some(true);
    let message = match (&reply.outcome, reply.status) {
        (Some(outcome), JobStatus::Finished) => hand_message(&outcome.receipt),
        _ => String::new(),
    };
    let envelope = match parse::<Hand>(&message) {
        Ok(envelope) => envelope.front,
        Err(error) => return Some(broken(ctx, reply, task, agent_ok, error)),
    };
    let push = push_of(&facts.receipt);
    Some(match envelope.answer {
        HandAnswer::Failed => AttemptResult::Failed,
        HandAnswer::Built => built(ctx, hand, task, agent_ok, &push, envelope),
        HandAnswer::BlockedScope => blocked(ctx, hand, task, agent_ok, &push, &envelope),
    })
}

fn built(
    ctx: &Ctx,
    job: &PoolJob,
    task: &str,
    agent_ok: bool,
    push: &Push<'_>,
    envelope: Hand,
) -> AttemptResult {
    if !agent_ok {
        disagree(ctx, task, job, "built", "the agent did not exit 0");
        return AttemptResult::Failed;
    }
    let failing: Vec<&str> = envelope
        .check
        .iter()
        .filter(|check| check.result == CheckResult::Fail)
        .map(|check| check.cmd.as_str())
        .collect();
    if !failing.is_empty() {
        ctx.exception(
            HAND_REPORTED_FAILURE,
            task,
            &format!(
                "hand job {}: the envelope says built but reports failing checks: {}",
                job.id,
                failing.join("; ")
            ),
        );
        return AttemptResult::Failed;
    }
    match push.word {
        "pushed" | "committed" => AttemptResult::Built {
            branch: push
                .branch
                .map(str::to_owned)
                .or_else(|| job.branch.clone())
                .unwrap_or_default(),
            envelope,
        },
        "no_changes" => AttemptResult::NoDefect,
        other => {
            let seen = if other.is_empty() {
                String::from("the runner reported no push result")
            } else {
                format!("the runner's push result is {other:?}")
            };
            disagree(ctx, task, job, "built", &seen);
            AttemptResult::Failed
        }
    }
}

fn blocked(
    ctx: &Ctx,
    job: &PoolJob,
    task: &str,
    agent_ok: bool,
    push: &Push<'_>,
    envelope: &Hand,
) -> AttemptResult {
    if !agent_ok {
        disagree(ctx, task, job, "blocked-scope", "the agent did not exit 0");
        return AttemptResult::Failed;
    }
    if matches!(push.word, "pushed" | "committed") {
        disagree(
            ctx,
            task,
            job,
            "blocked-scope",
            "the runner pushed commits; the branch is ignored",
        );
    }
    AttemptResult::BlockedScope(BlockedScope {
        paths: envelope.scope_paths.clone(),
        reason: envelope.scope_reason.clone().unwrap_or_default(),
        design: envelope.scope_design,
    })
}

/// A reply that did not parse: a correction while some remain and the agent
/// exited 0, `failed` otherwise. A crashed agent is a plain failed attempt.
fn broken(
    ctx: &Ctx,
    reply: &PoolJob,
    task: &str,
    agent_ok: bool,
    error: EnvelopeError,
) -> AttemptResult {
    if !agent_ok {
        return AttemptResult::Failed;
    }
    if correction_n(&reply.id) < MAX_CORRECTIONS {
        return AttemptResult::Correct { error };
    }
    ctx.exception(
        ENVELOPE_FAILED,
        task,
        &format!("hand job {}: {error}", reply.id),
    );
    AttemptResult::Failed
}

fn disagree(ctx: &Ctx, task: &str, job: &PoolJob, claimed: &str, seen: &str) {
    ctx.exception(
        ENVELOPE_DISAGREES,
        task,
        &format!(
            "hand job {}: envelope says {claimed}, runner says: {seen}; the runner wins",
            job.id
        ),
    );
}

/// The runner's `push:` line of a pool receipt.
struct Push<'a> {
    /// `pushed`, `committed`, `no_changes`, ... or empty when there is none.
    word: &'a str,
    /// The branch, when the line names one.
    branch: Option<&'a str>,
}

/// Read the `push: <word> [<branch> <sha>]` summary line. Only the pool's
/// summary lines count (everything before the first section), never the
/// hand's own text.
fn push_of(receipt: &str) -> Push<'_> {
    let summary = receipt
        .split("\n\n--- ")
        .next()
        .and_then(|head| head.split("hand's final message:").next())
        .unwrap_or_default();
    let line = summary
        .lines()
        .find_map(|line| line.trim().strip_prefix("push: "))
        .unwrap_or_default();
    let mut words = line.split_whitespace();
    Push {
        word: words.next().unwrap_or_default(),
        branch: words.next(),
    }
}

#[cfg(test)]
impl SliceMail for crate::pump::flow::fake_mail::FakeMail {
    fn resolve_want(&self, id: &str) -> Result<Option<String>, VivariumError> {
        Ok(Some(id.to_string()))
    }

    fn task_body(&self, task: &str) -> Result<String, VivariumError> {
        self.tasks
            .get(task)
            .map(|task| task.body.clone())
            .ok_or_else(|| VivariumError::Message(format!("fake mail: no task {task}")))
    }

    fn marked_replies(
        &self,
        task: &str,
        marker: &str,
    ) -> Result<Vec<(String, String)>, VivariumError> {
        use crate::pump::flow::fake_mail::Call;
        let found = self
            .tasks
            .get(task)
            .ok_or_else(|| VivariumError::Message(format!("fake mail: no task {task}")))?;
        Ok(found
            .replies
            .iter()
            .filter(|reply| reply.trim_start().starts_with(marker))
            .map(|reply| {
                let at = self.position(
                    |call| matches!(call, Call::Reply { handle, body } if handle == task && body == reply),
                );
                (format!("{:08}", at.unwrap_or_default()), reply.trim().to_owned())
            })
            .collect())
    }

    fn marked_replies_from(
        &self,
        task: &str,
        marker: &str,
        sender: &str,
    ) -> Result<Vec<(String, String)>, VivariumError> {
        if sender == self.identity {
            return self.marked_replies(task, marker);
        }
        let foreign = self.foreign.get(task).map(Vec::as_slice).unwrap_or(&[]);
        Ok(foreign
            .iter()
            .enumerate()
            .filter(|(_, (who, body))| who == sender && body.trim_start().starts_with(marker))
            .map(|(at, (_, body))| (format!("{at:08}"), body.trim().to_owned()))
            .collect())
    }
}

#[cfg(test)]
#[path = "slice_dispatch_test.rs"]
mod tests;

#[cfg(test)]
#[path = "slice_dispatch_check_test.rs"]
mod check_tests;
