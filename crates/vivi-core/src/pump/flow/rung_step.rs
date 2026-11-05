//! Rung steps: start rung jobs at the need-branch tip and collect them, with
//! one retry when the job is lost or times out.

use crate::pump::flow::collect_step::{RunCx, StepCollector};
use crate::pump::flow::decide;
use crate::pump::flow::def::{FAILED, FlowKind, NodeClass, SKIPPED};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::need_branch;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::tick::{OPERATOR_HOLD, raise_once};
use crate::pump::host::Ctx;
use crate::pump::jobs::rung_result::EnvFailure;
use crate::pump::jobs::{self, JobSpec, JobStatus, JobStore, PoolJob, rung, rung_result};
use vivi_error::VivariumError;

/// The rung a need-rung step runs.
const NEED_RUNG: &str = "need";
/// Attempts a rung step makes before a lost or timed-out job answers `failed`.
const MAX_ATTEMPTS: u32 = 2;
/// The class of the audit step whose payload may name a raised rung.

/// The rung job for `step`: [`rung::spec`] bound to the step task. N6 and N7
/// call this for rung-main (main tip, repair branch tip).
///
/// # Errors
/// Returns [`VivariumError::Config`] naming the rung and target when the rung
/// is not configured.
pub(crate) fn job(
    ctx: &Ctx,
    target: &str,
    rung: &str,
    tip: &str,
    step: &StepRow,
    attempt: u32,
) -> Result<JobSpec, VivariumError> {
    let mut spec = rung::spec(ctx, target, rung, tip, &step.task, attempt).ok_or_else(|| {
        VivariumError::Config(format!(
            "rung `{rung}`: expected a command in [pump.rungs] for target {target}; actual: not configured"
        ))
    })?;
    spec.task = Some(step.task.clone());
    Ok(spec)
}

/// Starts a need-rung step at the need-branch tip.
struct RungStart;

/// Starts a rung-raised step: the sticky raise of the run (see `decide::sticky_raise`), else `need`.
struct RaisedStart;

impl StepHandler for RungStart {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        if !cx.ctx.cfg.flow.stages.runs("need_rung") {
            return Ok(StartAction::skipped("need_rung", SKIPPED));
        }
        start_rung(cx, step, NEED_RUNG)
    }
}

impl StepHandler for RaisedStart {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let raised = decide::sticky_raise(cx.store, &cx.run.need, cx.run.run)?;
        start_rung(cx, step, raised.as_deref().unwrap_or(NEED_RUNG))
    }
}

/// The rung job for `rung` at the tip of the need branch.
fn start_rung(cx: &StartCx<'_>, step: &StepRow, rung: &str) -> Result<StartAction, VivariumError> {
    let need = &cx.run.need;
    let row = cx.store.need_branch(need)?.ok_or_else(|| {
        VivariumError::Other(format!(
            "rung {rung} for need {need}: expected a need_branches row; actual: none"
        ))
    })?;
    let tip = need_branch::tip(cx.ctx, cx.store, need)?.ok_or_else(|| {
        VivariumError::Other(format!(
            "rung {rung} for need {need}: expected a need-branch tip; actual: none"
        ))
    })?;
    let spec = job(cx.ctx, &row.target, rung, &tip, step, 1)?;
    Ok(StartAction::Job(Box::new(spec)))
}

/// Collects a rung job: the exit status decides `pass` or `fail`; a lost or
/// timed-out job runs once more.
pub(crate) struct RungCollector;

impl<S: JobStore, M: FlowMail> StepCollector<S, M> for RungCollector {
    fn collect(
        &self,
        cx: &mut RunCx<'_, S, M>,
        pool_job: &PoolJob,
        row: &StepJobRow,
        step: &StepRow,
        _class: &NodeClass,
    ) -> Result<(), VivariumError> {
        if let Some(env) = rung_result::env_failure(pool_job) {
            return hold_env(cx, pool_job, step, &env);
        }
        let lost = matches!(pool_job.status, JobStatus::Lost | JobStatus::Timeout);
        if lost && row.attempt < MAX_ATTEMPTS {
            return retry(cx, pool_job, row, step);
        }
        let (word, payload) = answer(pool_job);
        cx.advance_word(step, word, payload)
    }
}

/// The worker could not run the rung script: the job says nothing about the
/// code. Alarm once, queue nothing, and hold the step for the operator. The
/// rung's lost-retry cap and the flow attempt are untouched.
pub(crate) fn hold_env<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    job: &PoolJob,
    step: &StepRow,
    env: &EnvFailure,
) -> Result<(), VivariumError> {
    let rung = job
        .payload
        .get("rung")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("none");
    raise_once(
        cx.ctx,
        rung_result::ENV_FAILURE,
        &job.id,
        &format!(
            "rung {rung} for step {} task {} on worker {}: expected the rung script to run; \
             actual: {:?}, tool {}: {}; answer with vivi pump answer {} pass|fail|failed",
            step.node,
            step.task,
            job.pool.as_deref().unwrap_or("none"),
            env.kind,
            env.tool.as_deref().unwrap_or("none"),
            env.evidence,
            step.task
        ),
    );
    cx.store
        .set_step_state(&step.task, StepState::Waiting, Some(OPERATOR_HOLD))
}

/// The answer word and outcome payload of a finished rung job. The payload
/// carries `exit`, `tail` and the sorted `failing` ids: facts from cargo's own
/// output (N4a-29), never from model text. It also keeps the dispatch
/// provenance as the job saved it (`rung`, the exact `command`, `target`,
/// tested `tip`, source `job` and dispatch `task`) and the bounded `excerpt`,
/// so describing completed work never resolves the current configuration.
pub(crate) fn answer(pool_job: &PoolJob) -> (&'static str, serde_json::Value) {
    let result = rung_result::parse(pool_job);
    let word = match (result.pass, result.exit) {
        (true, _) => "pass",
        (false, Some(_)) => "fail",
        (false, None) => FAILED,
    };
    let payload = serde_json::json!({
        "exit": result.exit,
        "tail": result.tail,
        "failing": result.failing,
        "excerpt": result.excerpt,
        "rung": pool_job.payload.get("rung").and_then(serde_json::Value::as_str),
        "command": pool_job.command.as_deref(),
        "target": pool_job.target.as_deref(),
        "tip": pool_job.from_ref.as_deref(),
        "job": pool_job.id.as_str(),
        "task": pool_job.task.as_deref(),
    });
    (word, payload)
}

/// A value the payload saved, as text; none when the key is absent or null.
fn recorded<'a>(payload: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(serde_json::Value::as_str)
}

/// Render a saved rung answer as a diagnostic: the rung, exact command,
/// target, tested commit, exit, durable source job and dispatch task, then
/// the canonical failing ids, then the excerpt selection and its bounded
/// text. Reads only the stored payload; a value it does not carry is
/// reported as `not recorded`, never guessed from current configuration.
pub(crate) fn render_diagnostic(payload: &serde_json::Value) -> String {
    let mut out = String::new();
    for (label, key) in [
        ("rung: ", "rung"),
        ("command: ", "command"),
        ("target: ", "target"),
        ("tested: ", "tip"),
        ("source job: ", "job"),
    ] {
        out.push_str(label);
        out.push_str(recorded(payload, key).unwrap_or("not recorded"));
        out.push('\n');
    }
    out.push_str(&format!("exit: {}\n", exit_text(payload)));
    match recorded(payload, "task") {
        Some(task) => out.push_str(&format!(
            "task: {task} (dispatch task; task show --latest is not an immutable reference)\n"
        )),
        None => out.push_str("task: not recorded\n"),
    }
    out.push_str(&failing_text(payload));
    out.push_str(&excerpt_text(payload));
    out
}

/// The stored exit code, or `not recorded` when the job never reported one.
fn exit_text(payload: &serde_json::Value) -> String {
    payload
        .get("exit")
        .and_then(serde_json::Value::as_i64)
        .map_or_else(|| String::from("not recorded"), |code| code.to_string())
}

/// The canonical failing ids on their own lines. An empty set says that no
/// test was named: a build, size-check, timeout or lost-job failure is still
/// a failure and must not read as green.
fn failing_text(payload: &serde_json::Value) -> String {
    let ids = match payload.get("failing") {
        Some(serde_json::Value::Array(ids)) => ids,
        _ => return String::from("failing: not recorded\n"),
    };
    if ids.is_empty() {
        return String::from(
            "failing: no canonical failing test id (an empty set is not green; a build, \
             size-check, timeout or lost-job failure names no test)\n",
        );
    }
    let mut out = format!("failing: {} canonical test id(s)\n", ids.len());
    for id in ids.iter().filter_map(serde_json::Value::as_str) {
        out.push_str("  ");
        out.push_str(id);
        out.push('\n');
    }
    out
}

/// The excerpt's selection and cut counts, then its bounded text. Missing
/// evidence is reported as such rather than rendered as an empty green run.
fn excerpt_text(payload: &serde_json::Value) -> String {
    let Some(excerpt) = payload.get("excerpt").filter(|value| value.is_object()) else {
        return String::from("excerpt: not recorded\n");
    };
    let mode = excerpt
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let kept = excerpt
        .get("kept_lines")
        .and_then(serde_json::Value::as_u64);
    let source = excerpt
        .get("source_lines")
        .and_then(serde_json::Value::as_u64);
    let omitted = excerpt
        .get("omitted_lines")
        .and_then(serde_json::Value::as_u64);
    let truncated = excerpt
        .get("truncated")
        .and_then(serde_json::Value::as_bool);
    let mut out = match (kept, source, omitted, truncated) {
        (Some(kept), Some(source), Some(omitted), Some(truncated)) => format!(
            "excerpt: {mode}; kept {kept} of {source} source lines; omitted {omitted}; truncated {truncated}\n"
        ),
        _ => format!("excerpt: {mode}; selection counts not recorded\n"),
    };
    let text = excerpt
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if text.is_empty() {
        out.push_str("(no excerpt evidence)\n");
    } else {
        out.push_str(text);
        out.push('\n');
    }
    out
}

fn payload_str<'a>(job: &'a PoolJob, key: &str) -> Result<&'a str, VivariumError> {
    job.payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| VivariumError::Parse(format!("rung job {} has no `{key}` payload", job.id)))
}

/// Enqueue the next attempt of a lost rung job at the same tip.
fn retry<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    lost: &PoolJob,
    row: &StepJobRow,
    step: &StepRow,
) -> Result<(), VivariumError> {
    let attempt = row.attempt + 1;
    let target = payload_str(lost, "target")?;
    let rung = payload_str(lost, "rung")?;
    let tip = payload_str(lost, "tip")?;
    let mut spec = job(cx.ctx, target, rung, tip, step, attempt)?;
    spec.payload = lost.payload.clone();
    let new_row = row.next(&spec.id);
    new_row.stamp(&cx.run.need, &mut spec.payload);
    jobs::enqueue(cx.st, spec)?;
    cx.store.insert_step_job(&cx.run.need, &new_row)?;
    cx.store
        .set_step_state(&step.task, StepState::Running, None)
}

/// Register the build flow's rung handlers and the collector of every rung class.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(FlowKind::Build, NodeClass::RungNeed, Box::new(RungStart));
    registry.handler(
        FlowKind::Build,
        NodeClass::RungRaised,
        Box::new(RaisedStart),
    );
    for class in [
        NodeClass::RungNeed,
        NodeClass::RungMain,
        NodeClass::RungRelease,
        NodeClass::RungRaised,
    ] {
        registry.collector(FlowKind::Build, class, Box::new(RungCollector));
    }
}

#[cfg(test)]
#[path = "rung_step_test.rs"]
mod tests;
