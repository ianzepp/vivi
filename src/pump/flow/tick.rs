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

use crate::error::VivariumError;
use crate::pump::flow::advance::route::JoinInput;
use crate::pump::flow::advance::start::SENTINEL_NODE;
use crate::pump::flow::advance::{self, guard, terminal};
use crate::pump::flow::answer;
use crate::pump::flow::collect_step::RunCx;
use crate::pump::flow::cto_step::unchanged_text;
use crate::pump::flow::def::{FlowDef, FlowKind, FlowNode, NodeClass};
use crate::pump::flow::fanout::{self, is_slice_row};
use crate::pump::flow::handlers::{self, FlowMail, Registry};
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::kind;
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailReader, MailWriter, MailspacePort, NeedStatus};
use crate::pump::flow::outcome::{StartAction, StartCx};
use crate::pump::flow::step_error::{self, STEP_ERROR_LIMIT};
use crate::pump::flow::validate::validate;
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
/// Exception kind: a slice needs the slicer to rework the scopes.
pub const SCOPE_REWORK: &str = "flow-scope-rework";
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
    let registry = handlers::shipped::<S, MailspacePort>(ctx);
    tick_with(ctx, st, &store, &mut mail, &registry)
}

/// The tick over an explicit store, port and registry.
///
/// # Errors
/// Returns a [`VivariumError`] when the run lists cannot be read.
pub(crate) fn tick_with<S, M>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &mut M,
    registry: &Registry<S, M>,
) -> Result<(), VivariumError>
where
    S: JobStore,
    M: FlowMail,
{
    let def_for = |run: &RunRow| def_of(run);
    let decide = |node: &FlowNode, inputs: &[JoinInput]| registry.decide(store, node, inputs);
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
            RunCx {
                ctx,
                st: &mut *st,
                store,
                mail: &mut *mail,
                registry,
                decide: &decide,
                def,
                run: run.clone(),
            }
            .run(&unapplied)
        });
        boundary(ctx, &unit, result);
    }
    for phase in registry.phases() {
        boundary(ctx, phase.name(), phase.run(ctx, st, store, mail));
    }
    Ok(())
}

/// The flows checked at startup.
pub(crate) const VALIDATED: [FlowKind; 4] = [
    FlowKind::Build,
    FlowKind::Research,
    FlowKind::Release,
    FlowKind::Repair,
];

/// The shipped flows validate once per process; a failure is an exception.
fn validate_once(ctx: &Ctx) {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    for kind in VALIDATED {
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

pub(crate) fn raise_once(ctx: &Ctx, kind: &str, unit: &str, text: &str) {
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

impl<S, M> RunCx<'_, S, M>
where
    S: JobStore,
    M: FlowMail,
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
            if let Some(step) = rows.iter().find(|step| step.task == row.task) {
                let result = self.apply_job(id, row);
                self.step_unit(&row.task, &step.node, "collect", Some(id), result);
            }
        }
        self.release_scope_waits()?;
        self.settle_fanouts()?;
        self.start_steps()?;
        self.poll_waiting()?;
        if !self.still_live()? {
            return Ok(());
        }
        self.check_kind()?;
        self.check_overdue()
    }

    /// Whether this run is still `live` after the step work: a job applied
    /// this tick may have landed, restarted or withdrawn it, and an ended run
    /// has no kind left to check and no overdue job to report.
    fn still_live(&self) -> Result<bool, VivariumError> {
        const RUN_LIVE: &str = "live";
        let rows = self.store.runs_of(&self.run.need)?;
        Ok(rows
            .iter()
            .any(|row| row.run == self.run.run && row.state == RUN_LIVE))
    }

    /// Per-step error boundary: a failing unit never ends the run's tick.
    /// Failures below [`STEP_ERROR_LIMIT`] ticks in a row go to the run
    /// boundary; at the limit the step is held for the operator, its job (if
    /// any) is marked applied, and one alarm is raised.
    fn step_unit(
        &mut self,
        task: &str,
        node: &str,
        phase: &str,
        job: Option<&str>,
        result: Result<(), VivariumError>,
    ) {
        let unit = format!("{} r{}", self.run.need, self.run.run);
        let key = format!("{unit} {task}");
        let error = match result {
            Ok(()) => return step_error::note_success(self.ctx, &key),
            Err(error) => error,
        };
        let n = step_error::note_failure(self.ctx, &key);
        if n < STEP_ERROR_LIMIT {
            return boundary(self.ctx, &format!("{unit} {node}"), Err::<(), _>(error));
        }
        if let Some(id) = job {
            jobs::mark_applied(self.st, id, self.ctx.now());
        }
        let text = format!(
            "step {node} task {task} of need {} r{}: expected the {phase} to succeed; actual: {error} on {n} ticks in a row; held for `vivi pump answer {task} <word>`",
            self.run.need, self.run.run
        );
        if let Err(held) = step_error::hold(self.ctx, self.store, task, &text) {
            boundary(self.ctx, &format!("{unit} {node}"), Err::<(), _>(held));
        }
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
                let registry = self.registry;
                let kind = self.def.kind;
                let Some(collector) = registry.collector_for(kind, &class) else {
                    return Err(VivariumError::Parse(format!(
                        "no collector for node {} ({}) of the {} flow",
                        step.node,
                        class.name(),
                        kind.name()
                    )));
                };
                collector.collect(self, &job, row, &step, &class)?;
            }
        }
        jobs::mark_applied(self.st, id, self.ctx.now());
        Ok(())
    }

    // ---- fanouts, new steps, alarms ---------------------------------------

    fn settle_fanouts(&mut self) -> Result<(), VivariumError> {
        let Some(word) = fanout::settle(self.store, &*self.mail, &self.run)? else {
            return Ok(());
        };
        for step in self.open_rows()? {
            if self.class_of(&step.node) == Some(NodeClass::Fanout) {
                let payload = self.fanout_payload(&step, word)?;
                self.advance_word(&step, word, payload)?;
            }
        }
        Ok(())
    }

    /// The step payload of a settled fanout: an `unchanged` word says why.
    fn fanout_payload(
        &self,
        step: &StepRow,
        word: &str,
    ) -> Result<serde_json::Value, VivariumError> {
        if word != fanout::UNCHANGED {
            return Ok(serde_json::json!({"source": "fanout"}));
        }
        let reason = unchanged_text(
            self.ctx,
            self.store,
            &self.run.need,
            self.run.run,
            &step.node,
            &step.task,
        )?;
        Ok(serde_json::json!({"source": "fanout", "reason": reason}))
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
            let result = self.start_one(&step);
            self.step_unit(&step.task, &step.node, "start", None, result);
        }
        Ok(())
    }

    /// Start one pending step through its registered handler.
    fn start_one(&mut self, step: &StepRow) -> Result<(), VivariumError> {
        let registry = self.registry;
        let Some(node) = self.def.nodes.iter().find(|n| n.id == step.node) else {
            return Ok(());
        };
        let Some(handler) = registry.handler_for(self.def.kind, node) else {
            return Ok(());
        };
        let action = {
            let cx = StartCx {
                ctx: self.ctx,
                store: self.store,
                mail: &*self.mail,
                run: &self.run,
                def: &self.def,
            };
            handler.start(&cx, step)?
        };
        match action {
            StartAction::Job(spec) => self.enqueue_job(step, *spec),
            StartAction::OpenAndWait => {
                self.store
                    .set_step_state(&step.task, StepState::Waiting, None)
            }
            StartAction::Decide { word } => {
                self.advance_word(step, &word, serde_json::json!({"source": "code"}))
            }
            StartAction::Answer { word, payload } => self.advance_word(step, &word, payload),
            StartAction::Fanout {
                slicer_step,
                slices,
            } => {
                fanout::expand(
                    self.ctx,
                    self.store,
                    self.mail,
                    &self.run,
                    &slicer_step,
                    &slices,
                )?;
                self.store
                    .set_step_state(&step.task, StepState::Running, None)
            }
        }
    }

    /// Ask each waiting step's handler for an answer. Lock, park and
    /// milestone rows go straight to waiting, so polling is how their
    /// handlers answer. A held step is the operator's and is not polled.
    fn poll_waiting(&mut self) -> Result<(), VivariumError> {
        let rows = self.store.steps_of(&self.run.need, self.run.run)?;
        for step in rows {
            if step.state != StepState::Waiting
                || step.node == SENTINEL_NODE
                || is_slice_row(&step)
                || step.verdict.as_deref() == Some(OPERATOR_HOLD)
            {
                continue;
            }
            let result = self.poll_one(&step);
            self.step_unit(&step.task, &step.node, "poll", None, result);
        }
        Ok(())
    }

    /// Poll one waiting step's handler; an answer advances the step.
    fn poll_one(&mut self, step: &StepRow) -> Result<(), VivariumError> {
        let registry = self.registry;
        let Some(node) = self.def.nodes.iter().find(|n| n.id == step.node) else {
            return Ok(());
        };
        let Some(handler) = registry.handler_for(self.def.kind, node) else {
            return Ok(());
        };
        let word = {
            let cx = StartCx {
                ctx: self.ctx,
                store: self.store,
                mail: &*self.mail,
                run: &self.run,
                def: &self.def,
            };
            handler.poll(&cx, step)?
        };
        match word {
            Some(word) => self.advance_word(step, &word, serde_json::json!({"source": "poll"})),
            None => Ok(()),
        }
    }

    /// Enqueue a handler's first job for a step and mark the step running.
    fn enqueue_job(&mut self, step: &StepRow, mut spec: JobSpec) -> Result<(), VivariumError> {
        if spec.task.is_none() {
            spec.task = Some(step.task.clone());
        }
        let mut payload = match std::mem::take(&mut spec.payload) {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        for (key, value) in [
            ("need", serde_json::json!(self.run.need)),
            ("run", serde_json::json!(self.run.run)),
            ("node", serde_json::json!(step.node)),
            ("task", serde_json::json!(step.task)),
            ("attempt", serde_json::json!(1)),
            ("kind", serde_json::json!(self.run.kind)),
            (
                "class",
                serde_json::json!(
                    self.class_of(&step.node)
                        .map(|class| class.name())
                        .unwrap_or_default()
                ),
            ),
        ] {
            payload.insert(key.to_owned(), value);
        }
        spec.payload = serde_json::Value::Object(payload);
        let id = spec.id.clone();
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
        // No open record means no kind source (the run was landed or its need
        // closed): the next tick withdraws it, so say nothing here.
        let Some((_, body)) = MailReader::open_record(&*self.mail, &need)? else {
            return Ok(());
        };
        let marks = self.mail.need_marks(&need)?;
        let (now_kind, _) = kind::of(Some(&body), &marks);
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

#[cfg(test)]
#[path = "tick_test.rs"]
mod tests;

#[cfg(test)]
#[path = "tick_e2e_test.rs"]
mod e2e;

#[cfg(test)]
#[path = "tick_guard_test.rs"]
mod guard_tests;

#[cfg(test)]
#[path = "tick_registry_test.rs"]
mod registry_tests;

#[cfg(test)]
#[path = "tick_step_error_test.rs"]
mod step_error_tests;
