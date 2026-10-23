//! The flow engine's per-tick entry point.
//!
//! One tick, in order: validate the shipped flows (once per process), apply
//! the operator's pending answers, resume runs left `finishing`, then for each
//! live run (each inside its own error boundary, so a stuck run holds only its
//! own need): withdraw it if its need or pump record closed, otherwise run the
//! completion guard, collect finished jobs (roles, rungs, merges, slice
//! hands), release `scope-wait` slices, settle fanouts, start new steps, and
//! raise the kind-change and overdue-job alarms.
//!
//! Facts come from machines, judgment from models: a side-effect step with no
//! decisive receipt is checked against git ([`verify::check`]) before any
//! `failed` is answered. An unclear check holds the step for
//! `vivi pump answer`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

use crate::error::VivariumError;
use crate::pump::envelope::correct;
use crate::pump::envelope::roles::{FollowUp, Hand, RoleKind};
use crate::pump::flow::advance::{self, guard, route::JoinInput, terminal};
use crate::pump::flow::answer;
use crate::pump::flow::collect::{self, Collected};
use crate::pump::flow::def::{FAILED, FlowDef, FlowKind, FlowNode, NodeClass};
use crate::pump::flow::fanout::{self, VERDICT_MERGED, VERDICT_NO_DEFECT, is_slice_row};
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailReader, MailWriter, MailspacePort, NeedStatus, TaskStatus};
use crate::pump::flow::outcome::{Outcome, StartAction, StepHandlers};
use crate::pump::flow::run::SENTINEL_NODE;
use crate::pump::flow::scope::{
    BlockedScope, Resolution, ScopeMail, ScopeState, finish_grant, grant_held, resolve,
    wait_holders,
};
use crate::pump::flow::slice_dispatch::{
    self, AttemptResult, DEVIATIONS_MARKER, Retried, SliceMail,
};
use crate::pump::flow::validate::validate;
use crate::pump::flow::verify::{self, RunnerPublishReceipt, SideEffect, VerifyReq};
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// Marker on a follow-up reply posted on a built slice task.
pub const FOLLOW_UP_MARKER: &str = "pump-follow-up:";
/// Verdict a step carries while it waits for `vivi pump answer`.
pub const OPERATOR_HOLD: &str = "operator-hold";

/// Exception kind: a shipped flow failed validation.
pub const FLOW_INVALID: &str = "flow-invalid";
/// Exception kind: a run's tick failed.
pub const RUN_ERROR: &str = "flow-run-error";
/// Exception kind: a run stayed `finishing` too long.
pub const FINISHING_STUCK: &str = "flow-run-finishing-stuck";
/// Exception kind: git could not settle a step's side effect.
pub const SIDE_EFFECT_UNCLEAR: &str = "flow-side-effect-unclear";
/// Exception kind: a step job ran past its time budget.
pub const STEP_OVERDUE: &str = "flow-step-overdue";
/// Exception kind: a hand's built envelope carried follow-up work.
pub const FOLLOW_UP: &str = "flow-follow-up";
/// Exception kind: a slice needs the slicer to repair the scopes.
pub const SCOPE_REPAIR: &str = "flow-scope-repair";
/// Exception kind: the need's marks no longer match the run's kind.
pub const KIND_CHANGED: &str = "flow-kind-changed";
/// Exception kind: routing refused a collected word.
pub const WORD_REJECTED: &str = "flow-word-rejected";

/// Seconds a run may stay `finishing` before the alarm.
const FINISHING_STUCK_SECS: u64 = 600;
/// Seconds past `dispatched_at + budget` before a job is overdue.
const OVERDUE_GRACE_SECS: u64 = 600;

/// One tick of the flow engine against the project's pump store and mailspace.
///
/// # Errors
/// Returns a [`VivariumError`] when the store cannot be opened or its run
/// lists cannot be read. A failure inside one run is raised as an exception
/// and does not end the tick.
pub fn tick<S: JobStore>(ctx: &Ctx, st: &mut S) -> Result<(), VivariumError> {
    validate_once(ctx);
    if ctx.mailspace.is_empty() {
        return Ok(());
    }
    let Some(mailspace) = crate::pump::record::open_mailspace(ctx) else {
        return Ok(());
    };
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut mail = MailspacePort {
        mailspace,
        pump_identity: ctx.cfg.reply_from.clone(),
    };
    tick_with(ctx, st, &store, &mut mail, &StepHandlers::new())
}

/// The tick over an explicit store, port and step handlers.
///
/// # Errors
/// Returns a [`VivariumError`] when the run lists cannot be read.
pub(crate) fn tick_with<S, M>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &mut M,
    handlers: &StepHandlers,
) -> Result<(), VivariumError>
where
    S: JobStore,
    M: MailWriter + SliceMail + ScopeMail,
{
    let def_for = |run: &RunRow| def_of(run);
    boundary(
        ctx,
        "answers",
        answer::apply_pending(ctx, store, mail, &def_for, &decide),
    );
    for run in store.finishing_runs()? {
        resume_run(ctx, store, mail, &run);
    }
    let unapplied = unapplied_step_jobs(st, store)?;
    for run in store.live_runs()? {
        let unit = format!("{} r{}", run.need, run.run);
        let result = def_of(&run).and_then(|def| {
            Tick {
                ctx,
                st: &mut *st,
                store,
                mail: &mut *mail,
                handlers,
                def,
                run: run.clone(),
            }
            .run(&unapplied)
        });
        boundary(ctx, &unit, result);
    }
    Ok(())
}

/// The shipped flows validate once per process; a failure is an exception.
fn validate_once(ctx: &Ctx) {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    for kind in [FlowKind::Build, FlowKind::Research, FlowKind::Release] {
        let verdict = load(kind)
            .map_err(|error| vec![error.to_string()])
            .and_then(|def| validate(&def));
        if let Err(problems) = verdict {
            ctx.exception(FLOW_INVALID, kind.name(), &problems.join("; "));
        }
    }
}

fn def_of(run: &RunRow) -> Result<FlowDef, VivariumError> {
    let kind = FlowKind::parse(&run.kind).ok_or_else(|| {
        VivariumError::Parse(format!(
            "need {} r{}: unknown flow kind `{}`",
            run.need, run.run, run.kind
        ))
    })?;
    load(kind)
}

/// No code decision has a rule yet: a join or raise check that routing reaches
/// is an error held on its run.
fn decide(node: &FlowNode, _inputs: &[JoinInput]) -> Result<String, VivariumError> {
    Err(VivariumError::Parse(format!(
        "decision node `{}` has no decision rule yet",
        node.id
    )))
}

/// Log a failed unit of the tick and raise it, at most once per distinct error
/// per [`crate::pump::tick_steps::RERAISE_SECS`].
fn boundary<T>(ctx: &Ctx, unit: &str, result: Result<T, VivariumError>) {
    if let Err(error) = result {
        let text = error.to_string();
        ctx.log(&format!("flow tick: {unit} failed: {text}"));
        let key = format!("{}\u{0}flow\u{0}{unit}", ctx.home.display());
        if crate::pump::tick_steps::should_raise(&key, &text, ctx.now()) {
            ctx.exception(RUN_ERROR, unit, &text);
        }
    }
}

/// True the first time `(kind, unit)` is seen in this process for this home.
fn first_time(ctx: &Ctx, kind: &str, unit: &str) -> bool {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let mut seen = SEEN
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    seen.insert(format!("{}\u{0}{kind}\u{0}{unit}", ctx.home.display()))
}

fn raise_once(ctx: &Ctx, kind: &str, unit: &str, text: &str) {
    if first_time(ctx, kind, unit) {
        ctx.exception(kind, unit, text);
    }
}

/// Complete a run left `finishing`; past [`FINISHING_STUCK_SECS`] raise the
/// stuck alarm once, naming the need, the run and the failure.
fn resume_run<M: MailWriter + MailReader>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    run: &RunRow,
) {
    let Err(error) = terminal::resume(ctx, store, mail, run) else {
        return;
    };
    let unit = format!("{} r{}", run.need, run.run);
    ctx.log(&format!("flow tick: resume of {unit} failed: {error}"));
    let since = match store.run_state_at(&run.need, run.run) {
        Ok(Some(at)) => u64::try_from(at).ok(),
        _ => None,
    };
    if since.is_some_and(|at| ctx.now().saturating_sub(at) > FINISHING_STUCK_SECS) {
        raise_once(
            ctx,
            FINISHING_STUCK,
            &unit,
            &format!(
                "need {} run {} is stuck finishing: {error}",
                run.need, run.run
            ),
        );
    }
}

/// Terminal jobs not yet applied that belong to a step: `(job id, row)`.
fn unapplied_step_jobs<S: JobStore>(
    st: &S,
    store: &PumpStore,
) -> Result<Vec<(String, StepJobRow)>, VivariumError> {
    let mut found = Vec::new();
    for (id, job) in st.jobs() {
        if job.status.is_terminal()
            && job.applied_at.is_none()
            && let Some(row) = store.step_job(id)?
        {
            found.push((id.clone(), row));
        }
    }
    Ok(found)
}

/// One live run's tick.
struct Tick<'a, S, M> {
    ctx: &'a Ctx,
    st: &'a mut S,
    store: &'a PumpStore,
    mail: &'a mut M,
    handlers: &'a StepHandlers,
    def: FlowDef,
    run: RunRow,
}

impl<S, M> Tick<'_, S, M>
where
    S: JobStore,
    M: MailWriter + SliceMail + ScopeMail,
{
    fn run(&mut self, unapplied: &[(String, StepJobRow)]) -> Result<(), VivariumError> {
        let need = self.run.need.clone();
        let pump = self.ctx.cfg.reply_from.clone();
        // A closed need is a withdrawal, unless a join closed it: a foreign
        // close of the sentinel and the last open step leaves every unit
        // done. The guard tells the two apart (it reopens only when no run
        // task is still open), so it runs first while the pump record is
        // open, and the need state is read after it.
        let record_open = MailReader::open_record(&*self.mail, &need)?.is_some();
        if record_open {
            guard::pass(self.ctx, self.store, self.mail, &self.run, &pump)?;
        }
        if !record_open || self.mail.need_state(&need)? == NeedStatus::Done {
            return advance::withdraw(self.store, self.mail, &self.run);
        }
        let rows = self.store.steps_of(&need, self.run.run)?;
        for (id, row) in unapplied {
            if rows.iter().any(|step| step.task == row.task) {
                self.apply_job(id, row)?;
            }
        }
        self.release_scope_waits()?;
        self.settle_fanouts()?;
        self.start_steps()?;
        self.check_kind()?;
        self.check_overdue()
    }

    // ---- finished jobs -----------------------------------------------------

    fn apply_job(&mut self, id: &str, row: &StepJobRow) -> Result<(), VivariumError> {
        let Some(job) = self.st.jobs().get(id).cloned() else {
            return Ok(());
        };
        let step = self.store.step_by_task(&row.task)?;
        if let Some(step) = step.filter(|s| {
            matches!(
                s.state,
                StepState::Pending | StepState::Running | StepState::Waiting
            )
        }) {
            if is_slice_row(&step) {
                self.slice_job(&job, row, &step)?;
            } else if let Some(class) = self.class_of(&step.node) {
                self.step_job(&job, row, &step, &class)?;
            }
        }
        jobs::mark_applied(self.st, id, self.ctx.now());
        Ok(())
    }

    fn class_of(&self, node: &str) -> Option<NodeClass> {
        self.def
            .nodes
            .iter()
            .find(|n| n.id == node)
            .map(|n| n.class.clone())
    }

    fn advance_word(
        &mut self,
        step: &StepRow,
        word: &str,
        payload: serde_json::Value,
    ) -> Result<(), VivariumError> {
        let outcome = Outcome {
            need: self.run.need.clone(),
            run: self.run.run,
            node: step.node.clone(),
            task: step.task.clone(),
            word: word.to_owned(),
            payload,
        };
        let advanced = advance::advance(
            self.ctx, self.store, self.mail, &self.def, &outcome, &decide,
        )?;
        if let Some(reason) = advanced.routed.rejected {
            self.ctx.exception(WORD_REJECTED, &step.task, &reason);
        }
        Ok(())
    }

    /// A finished job of a flow node: a role, a rung or a merge.
    fn step_job(
        &mut self,
        job: &PoolJob,
        row: &StepJobRow,
        step: &StepRow,
        class: &NodeClass,
    ) -> Result<(), VivariumError> {
        match class {
            NodeClass::MergeMain => match crate::pump::jobs::merge_main::classify(job) {
                Some(crate::pump::jobs::merge_main::MergeMainResult::Merged(sha)) => {
                    self.advance_word(step, "merged", merged_payload(&sha))
                }
                Some(crate::pump::jobs::merge_main::MergeMainResult::Conflict) => {
                    self.advance_word(step, "conflict", serde_json::json!({}))
                }
                _ => self.verify_step(job, step),
            },
            NodeClass::RungNeed
            | NodeClass::RungMain
            | NodeClass::RungRelease
            | NodeClass::RungRaised => {
                let result = crate::pump::jobs::rung_result::parse(job);
                let word = match (result.pass, result.exit) {
                    (true, _) => "pass",
                    (false, Some(_)) => "fail",
                    (false, None) => FAILED,
                };
                self.advance_word(
                    step,
                    word,
                    serde_json::json!({"exit": result.exit, "tail": result.tail}),
                )
            }
            _ => self.role_job(job, row, step, class),
        }
    }

    fn role_job(
        &mut self,
        job: &PoolJob,
        row: &StepJobRow,
        step: &StepRow,
        class: &NodeClass,
    ) -> Result<(), VivariumError> {
        let expected = job.publish_ref.clone().unwrap_or_default();
        let collected = collect::collect(self.ctx, self.mail, job, row, class, &expected);
        match collected {
            Collected::Outcome(word, payload) => self.advance_word(step, &word, payload),
            Collected::Failed => {
                self.advance_word(step, FAILED, serde_json::json!({"job": job.id}))
            }
            Collected::Verify => self.verify_step(job, step),
            Collected::Retry if job.kind == correct::KIND => {
                self.advance_word(step, FAILED, serde_json::json!({"job": job.id}))
            }
            Collected::Retry => self.rerun(job, row, step),
            Collected::Correct { error } => {
                let kind = RoleKind::from_class(class).ok_or_else(|| {
                    VivariumError::Parse(format!("node {} has no role kind", step.node))
                })?;
                self.correct(job, row, kind, &error)
            }
        }
    }

    /// Queue a correction job for the broken reply of `job`.
    fn correct(
        &mut self,
        job: &PoolJob,
        row: &StepJobRow,
        kind: RoleKind,
        error: &crate::pump::envelope::EnvelopeError,
    ) -> Result<(), VivariumError> {
        let n = correct::correction_n(&job.id) + 1;
        let spec = correct::spec(&self.run.need, row, &job.role, kind, job, error, n);
        correct::enqueue(self.st, self.store, &self.run.need, row, spec)
    }

    /// Run a role step's job again as the next attempt.
    fn rerun(
        &mut self,
        job: &PoolJob,
        row: &StepJobRow,
        step: &StepRow,
    ) -> Result<(), VivariumError> {
        let attempt = row.attempt + 1;
        let id = format!("{}-a{attempt}", correct::base_id(&job.id));
        let mut spec = JobSpec::new(
            id.clone(),
            &job.kind,
            &job.role,
            job.subject.clone(),
            job.body.clone(),
        );
        spec.task.clone_from(&job.task);
        spec.target.clone_from(&job.target);
        spec.from_ref.clone_from(&job.from_ref);
        spec.branch.clone_from(&job.branch);
        spec.publish_ref.clone_from(&job.publish_ref);
        spec.pool_candidates.clone_from(&job.pool_candidates);
        spec.local_only = job.local_only;
        spec.read_only = job.read_only;
        spec.budget = job.budget;
        spec.payload = job.payload.clone();
        jobs::enqueue(self.st, spec)?;
        self.store.insert_step_job(
            &self.run.need,
            &StepJobRow {
                job: id,
                run: row.run,
                node: row.node.clone(),
                task: row.task.clone(),
                attempt,
            },
        )?;
        self.store
            .set_step_state(&step.task, StepState::Running, None)
    }

    /// A side-effect step with no decisive receipt: git decides.
    fn verify_step(&mut self, job: &PoolJob, step: &StepRow) -> Result<(), VivariumError> {
        let receipt = job
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.runner_publish())
            .filter(|publish| publish.succeeded())
            .map(|publish| RunnerPublishReceipt {
                sha: publish.remote_sha,
            });
        let remote_ref = job.publish_ref.clone().unwrap_or_default();
        let req = VerifyReq {
            target: job.target.clone().unwrap_or_default(),
            remote_ref: remote_ref.clone(),
            before: job.from_ref.clone(),
            must_contain: job
                .publish_contains
                .clone()
                .or_else(|| job.publish_expect.clone())
                .unwrap_or_default(),
            receipt,
        };
        match verify::check(self.ctx, &req) {
            SideEffect::Verified { sha } => self.advance_word(step, "merged", merged_payload(&sha)),
            SideEffect::Absent => {
                self.advance_word(step, FAILED, serde_json::json!({"job": job.id}))
            }
            SideEffect::Unclear { reason } => {
                self.ctx.exception(
                    SIDE_EFFECT_UNCLEAR,
                    &step.task,
                    &format!(
                        "step {} task {} job {} ref {remote_ref}: {reason}; answer with `vivi pump answer`",
                        step.node, step.task, job.id
                    ),
                );
                self.store
                    .set_step_state(&step.task, StepState::Waiting, Some(OPERATOR_HOLD))
            }
        }
    }

    // ---- slices ------------------------------------------------------------

    fn slice_job(
        &mut self,
        job: &PoolJob,
        row: &StepJobRow,
        step: &StepRow,
    ) -> Result<(), VivariumError> {
        let hand = if correct::correction_n(&job.id) > 0 {
            self.st.jobs().get(correct::base_id(&job.id)).cloned()
        } else {
            None
        };
        let hand = hand.as_ref().unwrap_or(job);
        let Some(result) = slice_dispatch::classify_corrected(self.ctx, hand, job) else {
            return Ok(());
        };
        let need = self.run.need.clone();
        match result {
            AttemptResult::Built { envelope, .. } => self.built(step, &envelope),
            AttemptResult::NoDefect => {
                self.mail
                    .close_task(&step.task, VERDICT_NO_DEFECT, None, None)?;
                self.store
                    .set_step_state(&step.task, StepState::Done, Some(VERDICT_NO_DEFECT))
            }
            AttemptResult::Failed => {
                let retried = slice_dispatch::retry(
                    self.ctx,
                    self.st,
                    self.store,
                    &*self.mail,
                    &need,
                    step,
                    hand,
                    row.attempt,
                )?;
                if retried == Retried::Exhausted {
                    self.ctx
                        .log(&format!("slice {} exhausted its attempts", step.task));
                }
                Ok(())
            }
            AttemptResult::Correct { error } => self.correct(job, row, RoleKind::Hand, &error),
            AttemptResult::BlockedScope(request) => self.blocked(step, row, &request),
        }
    }

    /// A built slice stays open (the merge is a later stage); its deviations
    /// and follow-up work are posted for the next hands and the slicer.
    fn built(&mut self, step: &StepRow, envelope: &Hand) -> Result<(), VivariumError> {
        if !envelope.deviations.is_empty() {
            let text = envelope.deviations.join("; ");
            self.mail
                .reply(&step.task, &marked_reply_text(DEVIATIONS_MARKER, &text))?;
        }
        if !envelope.follow_up.is_empty() {
            #[derive(Serialize)]
            struct FollowUps<'a> {
                follow_up: &'a [FollowUp],
            }
            let toml = toml::to_string(&FollowUps {
                follow_up: &envelope.follow_up,
            })
            .map_err(|error| VivariumError::Other(format!("follow-up encoding: {error}")))?;
            self.mail.reply(
                &step.task,
                &marked_reply_text(FOLLOW_UP_MARKER, &format!("\n{toml}")),
            )?;
            self.ctx.exception(
                FOLLOW_UP,
                &step.task,
                &format!(
                    "{} reported {} follow-up item(s)",
                    step.node,
                    envelope.follow_up.len()
                ),
            );
        }
        Ok(())
    }

    fn blocked(
        &mut self,
        step: &StepRow,
        row: &StepJobRow,
        request: &BlockedScope,
    ) -> Result<(), VivariumError> {
        let resolution = resolve(self.store, self.mail, self.run.run, &step.task, request)?;
        match resolution {
            Resolution::Widen(_) => self.redispatch(step, row.attempt),
            Resolution::WaitOn(_) => {
                self.store
                    .set_step_state(&step.task, StepState::Waiting, None)
            }
            Resolution::Repair(reason) => {
                self.store.set_scope_state(&step.task, ScopeState::Repair)?;
                self.ctx.exception(SCOPE_REPAIR, &step.task, &reason);
                Ok(())
            }
        }
    }

    fn redispatch(&mut self, step: &StepRow, attempt: u32) -> Result<(), VivariumError> {
        slice_dispatch::redispatch_scope(
            self.ctx,
            self.st,
            self.store,
            &*self.mail,
            &self.run.need,
            step,
            attempt,
        )
        .map(|_| ())
    }

    /// A `scope-wait` slice whose holders all closed goes out again.
    fn release_scope_waits(&mut self) -> Result<(), VivariumError> {
        for step in self.store.steps_of(&self.run.need, self.run.run)? {
            if !is_slice_row(&step) || self.store.scope_state(&step.task)? != Some(ScopeState::Wait)
            {
                continue;
            }
            let mut all_closed = true;
            for holder in wait_holders(self.store, &step.task)? {
                let state = self.mail.task_state(&holder)?;
                let landed = matches!(
                    state.verdict.as_deref(),
                    Some(VERDICT_MERGED | VERDICT_NO_DEFECT)
                );
                all_closed &= state.status == TaskStatus::Done && landed;
            }
            if all_closed {
                grant_held(self.store, &step.task)?;
                let attempt = self.current_attempt(&step.task);
                self.redispatch(&step, attempt)?;
                finish_grant(self.store, &step.task)?;
            }
        }
        Ok(())
    }

    /// The attempt of the newest hand job on `task` (1 when none is known).
    fn current_attempt(&self, task: &str) -> u32 {
        self.st
            .jobs()
            .values()
            .filter(|job| job.task.as_deref() == Some(task) && job.kind == slice_dispatch::KIND)
            .filter_map(|job| job.payload.get("attempt")?.as_u64())
            .map(|attempt| u32::try_from(attempt).unwrap_or(u32::MAX))
            .max()
            .unwrap_or(1)
    }

    // ---- fanouts, new steps, alarms ---------------------------------------

    fn settle_fanouts(&mut self) -> Result<(), VivariumError> {
        let Some(word) = fanout::settle(self.store, &*self.mail, &self.run)? else {
            return Ok(());
        };
        for step in self.open_rows()? {
            if self.class_of(&step.node) == Some(NodeClass::Fanout) {
                self.advance_word(&step, word, serde_json::json!({"source": "fanout"}))?;
            }
        }
        Ok(())
    }

    /// Open step rows (not sentinel, not slices) in index order.
    fn open_rows(&self) -> Result<Vec<StepRow>, VivariumError> {
        Ok(self
            .store
            .steps_of(&self.run.need, self.run.run)?
            .into_iter()
            .filter(|row| row.node != SENTINEL_NODE && !is_slice_row(row))
            .filter(|row| matches!(row.state, StepState::Pending | StepState::Running))
            .collect())
    }

    fn start_steps(&mut self) -> Result<(), VivariumError> {
        for step in self.open_rows()? {
            if step.state != StepState::Pending {
                continue;
            }
            let Some(class) = self.class_of(&step.node) else {
                continue;
            };
            let Some(handler) = self.handlers.get(&class) else {
                continue;
            };
            match handler.start(&step)? {
                StartAction::EnqueueJob { kind, brief } => {
                    self.enqueue_first(&step, &kind, &brief)?
                }
                StartAction::OpenAndWait => {
                    self.store
                        .set_step_state(&step.task, StepState::Waiting, None)?;
                }
                StartAction::Decide { word } => {
                    self.advance_word(&step, &word, serde_json::json!({"source": "code"}))?;
                }
            }
        }
        Ok(())
    }

    fn enqueue_first(
        &mut self,
        step: &StepRow,
        kind: &str,
        brief: &str,
    ) -> Result<(), VivariumError> {
        let id = format!("step-{}-a1", step.task);
        let mut spec = JobSpec::new(
            id.clone(),
            kind,
            kind,
            format!("{} ({} r{})", step.node, self.run.need, self.run.run),
            brief,
        );
        spec.task = Some(step.task.clone());
        spec.budget = self.ctx.cfg.dispatch.budget;
        spec.payload = serde_json::json!({
            "need": self.run.need,
            "run": self.run.run,
            "node": step.node,
            "task": step.task,
            "attempt": 1,
        });
        jobs::enqueue(self.st, spec)?;
        self.store.insert_step_job(
            &self.run.need,
            &StepJobRow {
                job: id,
                run: self.run.run,
                node: step.node.clone(),
                task: step.task.clone(),
                attempt: 1,
            },
        )?;
        self.store
            .set_step_state(&step.task, StepState::Running, None)
    }

    /// Raise once when the need's marks no longer name the run's kind.
    fn check_kind(&mut self) -> Result<(), VivariumError> {
        let need = self.run.need.clone();
        let from_record = MailReader::open_record(&*self.mail, &need)?
            .and_then(|(_, body)| {
                body.lines().find_map(|line| {
                    line.trim()
                        .strip_prefix("kind:")
                        .map(str::trim)
                        .map(str::to_owned)
                })
            })
            .and_then(|word| FlowKind::parse(&word));
        let marks = self.mail.need_marks(&need)?;
        let has = |mark: &str| marks.iter().any(|m| m == mark);
        let now_kind = from_record.unwrap_or(if has("research") {
            FlowKind::Research
        } else if has("release") {
            FlowKind::Release
        } else {
            FlowKind::Build
        });
        if now_kind.name() != self.run.kind {
            raise_once(
                self.ctx,
                KIND_CHANGED,
                &format!("{need} r{}", self.run.run),
                &format!(
                    "need {need} now reads as {}; run r{} stays {} (no action taken)",
                    now_kind.name(),
                    self.run.run,
                    self.run.kind
                ),
            );
        }
        Ok(())
    }

    /// A dispatched step job past its budget plus grace raises once.
    fn check_overdue(&mut self) -> Result<(), VivariumError> {
        let now = self.ctx.now();
        let late: Vec<PoolJob> = self
            .st
            .jobs()
            .values()
            .filter(|job| job.status == JobStatus::Dispatched)
            .filter(|job| {
                job.dispatched_at
                    .is_some_and(|at| now > at + job.budget + OVERDUE_GRACE_SECS)
            })
            .cloned()
            .collect();
        for job in late {
            let Some(row) = self.store.step_job(&job.id)? else {
                continue;
            };
            if row.run != self.run.run || self.store.step_by_task(&row.task)?.is_none() {
                continue;
            }
            raise_once(
                self.ctx,
                STEP_OVERDUE,
                &job.id,
                &format!(
                    "step {} task {} job {} pool {} dispatched at {}: budget {}s + {OVERDUE_GRACE_SECS}s grace passed",
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
}

/// The text of a pump-marked reply: `<marker> t=<unix micros> <body>`, so
/// replies sort in the order they were written.
fn marked_reply_text(marker: &str, body: &str) -> String {
    crate::mailspace::marked_line(marker, body)
}

fn merged_payload(sha: &str) -> serde_json::Value {
    serde_json::json!({"sha": sha, "commit": sha})
}

#[cfg(test)]
#[path = "tick_test.rs"]
mod tests;

#[cfg(test)]
#[path = "tick_e2e_test.rs"]
mod e2e;

#[cfg(test)]
#[path = "tick_guard_test.rs"]
mod guard_tests;
