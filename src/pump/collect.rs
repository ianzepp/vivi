//! Dispatch reconcile parsing and outcome classification.
//!
//! `collect` reconciles the pool once per tick, classifies every
//! in-flight unit (deliver, fail, split, no-defect), expires unanswered split
//! planners, and returns the gate task's receipt when the gate finished; the
//! caller hands that receipt to the gate module.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::VivariumError;
use crate::pump::attempt_record::{self, Ending};
use crate::pump::envfail;
use crate::pump::host::{Ctx, exit_receipt_path, shell_quote, shell_split};
use crate::pump::state::{Attempt, State, Status, Unit};
use crate::pump::transient;

/// Seconds a unit may stay unreported by reconcile before the pool-status
/// fallback decides.
const RECONCILE_GRACE: u64 = 90;
/// Receipt marker of a worker that found nothing to fix.
const NO_DEFECT_MARKER: &str = "VERDICT: no-defect";
/// Seconds a gate task may stay unreported before it is finished from its
/// receipt.
const GATE_GRACE: u64 = 2400;
/// Split-planner exits without a plan before the unit is parked stuck.
const SPLIT_RETRY_MAX: u32 = 3;

/// `SPLIT_PROMPT`; `{home}` and `{uid}` are substituted.
const SPLIT_PROMPT: &str = r#"One unit ran out of time on a worker, so it is too big. Split it.

The unit is in {home}/resplit/{uid}.json (id, need, target, title, brief, write_scope, proof, and what the worker last said). Read the code it names. Write {home}/plans/{uid}.split.json (write .tmp, then mv) in the plan format of your system prompt, with these extra rules:
- top level: {"need": "<the unit's need>", "replaces": "{uid}", "verdict": "units", "reason": "...", "units": [...]}
- unit ids are "{uid}.1", "{uid}.2", ... ; two to four units; each one must be finishable in 5 minutes by a mid-level model
- retain the original unit's target for every replacement unit
- chain them with `after` when they touch the same file
- if the worker's last message shows the work is in fact already done or not a defect, write "units": [] and say why in "reason"
Your final message is one line.
"#;

/// One unit handed to the split planner (`resplit/<uid>.json`).
#[derive(Serialize)]
struct ResplitRecord {
    /// Unit handle.
    id: String,
    /// Owning need handle.
    need: String,
    /// Repository target the replacement units must keep.
    target: String,
    /// One-line title.
    title: String,
    /// Full brief the unit was dispatched with.
    brief: String,
    /// Files the unit may write.
    write_scope: Vec<String>,
    /// Proof commands the unit must pass.
    proof: Vec<String>,
    /// The unit's kind; empty means `default_kind`.
    tools: String,
    /// Last 3000 chars of the text that triggered the split.
    worker_said: String,
}

/// Parse `vivi pool reconcile --porcelain` output into task handles mapped
/// to `(state, detail)`.
///
/// Only lines whose first word is `reconciled` or `collected` and that carry
/// at least six shell words count; word 1 is the task, word 4 the state,
/// word 5 the detail. A later line for the same task wins.
#[must_use]
pub fn parse_reconcile(out: &str) -> BTreeMap<String, (String, String)> {
    let mut seen = BTreeMap::new();
    for line in out.lines() {
        let words = shell_split(line);
        if words.len() >= 6 && (words[0] == "reconciled" || words[0] == "collected") {
            seen.insert(words[1].clone(), (words[4].clone(), words[5].clone()));
        }
    }
    seen
}

/// The last 6000 chars of `vivi task show <task> --latest`.
#[must_use]
pub fn receipt(ctx: &Ctx, task: &str) -> String {
    tail(&receipt_full(ctx, task), 6000)
}

/// All of `vivi task show <task> --latest`: the pool's summary lines (the
/// agent's exit code among them) come first, the agent's own final message
/// after them.
#[must_use]
pub fn receipt_full(ctx: &Ctx, task: &str) -> String {
    ctx.vivi(&format!("task show {task} --latest"), 60).text
}

/// Write the planner prompt to `<home>/<dir>/<id>.prompt` and start the
/// configured planner detached: stdin is the prompt file, output lands in
/// `<home>/<dir>/<id>.out`, the working directory is the pump home
/// Used by `split`, the board and gate fixes. Returns the detached child's
/// pid, or `None` when it could not be started.
pub fn spawn_planner(ctx: &Ctx, dir: &str, id: &str, prompt: &str) -> Option<u32> {
    let dir_path = ctx.home.join(dir);
    drop(fs::create_dir_all(&dir_path));
    let prompt_path = dir_path.join(format!("{id}.prompt"));
    drop(fs::write(&prompt_path, prompt));
    let command = ctx
        .cfg
        .planner
        .run
        .replace("{charter}", &ctx.cfg.planner.charter);
    let command = if ctx.shell_prefix.is_empty() {
        command
    } else {
        format!("{}; {command}", ctx.shell_prefix)
    };
    let out = dir_path.join(format!("{id}.out"));
    drop(fs::remove_file(exit_receipt_path(&out)));
    ctx.host
        .spawn(&command, Some(&ctx.home), Some(&prompt_path), &out)
}

/// Record a failed attempt: append the history entry, clear the task, and
/// either return the unit to `pending` or park it as `stuck` with an
/// exception once `max_attempts` is reached. The attempt record is posted on
/// the need thread (see [`attempt_record`]), fetching the task receipt.
///
/// A transient-class failure (see [`transient::classify`]) backs off and
/// retries instead of counting toward `max_attempts`, up to
/// `transient_max` retries; past that cap the unit takes the normal path.
pub fn fail(ctx: &Ctx, st: &mut State, uid: &str, why: &str, detail: &str) {
    fail_with(ctx, st, uid, why, detail, None);
}

/// [`fail`] with the task's full receipt when the caller already has it.
fn fail_with(ctx: &Ctx, st: &mut State, uid: &str, why: &str, detail: &str, receipt: Option<&str>) {
    let now = ctx.now();
    let max = ctx.cfg.dispatch.max_attempts;
    let cap = ctx.cfg.dispatch.transient_max;
    let backoff = ctx.cfg.dispatch.transient_backoff;
    let Some(unit) = st.units.get_mut(uid) else {
        return;
    };
    unit.history.push(Attempt {
        attempt: unit.attempts,
        model: unit.model.clone(),
        why: why.to_owned(),
        detail: tail(detail, 2500),
        at: now,
        branch: unit.branch.clone(),
        task: unit.task.clone(),
    });
    let attempts = unit.attempts;
    attempt_record::post(ctx, st, uid, &Ending::failed(why, detail, receipt));
    let Some(unit) = st.units.get_mut(uid) else {
        return;
    };
    unit.task = None;
    let mut transient = false;
    if let Some(class) = transient::classify(why, detail) {
        transient = transient_retry(unit, now, cap, backoff);
        if transient {
            ctx.log(&format!(
                "transient-retry {uid} {class} {}/{cap}",
                unit.transient_retries
            ));
        }
    }
    if !transient {
        let stuck = attempts.saturating_sub(unit.transient_retries) >= max;
        unit.status = if stuck {
            Status::Stuck
        } else {
            Status::Pending
        };
        if stuck {
            ctx.exception("stuck", uid, &format!("{why}: {}", tail(detail, 300)));
            crate::pump::quarantine::repair_stuck(ctx, st, uid);
        }
    }
    ctx.log(&format!("fail {uid} attempt={attempts} {why}"));
}

/// Back off and retry a transient-class failure: return the unit to
/// `pending`, count the retry, and set the clock it may be redispatched at
/// (`backoff * 2^retries_before`). Returns `false` at the retry cap, leaving
/// the unit for the normal stuck/pending decision; the caller logs the class.
fn transient_retry(unit: &mut Unit, now: u64, cap: u32, backoff: u64) -> bool {
    if unit.transient_retries >= cap {
        return false;
    }
    let retries_before = unit.transient_retries;
    unit.transient_retries = retries_before.saturating_add(1);
    let wait = backoff.saturating_mul(2u64.saturating_pow(retries_before));
    unit.retry_not_before = Some(now.saturating_add(wait));
    unit.status = Status::Pending;
    true
}

/// A host-class failure (see [`envfail`]): the attempt never measured the
/// unit, so undo the dispatch instead of failing it — drop the attempt
/// branch, restore the attempt (no history entry, no record posted, no
/// quarantine repair), count the failure toward pausing the unit's pool,
/// and log.
fn host_fail(ctx: &Ctx, st: &mut State, uid: &str, host: &envfail::HostFailure, receipt: &str) {
    drop_attempt_branch(ctx, st, uid);
    let now = ctx.now();
    let pool = {
        let Some(unit) = st.units.get_mut(uid) else {
            return;
        };
        unit.attempts = unit.attempts.saturating_sub(1);
        unit.status = Status::Pending;
        unit.task = None;
        unit.branch = None;
        unit.since = now;
        unit.since_dispatch = None;
        unit.pool.clone()
    };
    let evidence = format!(
        "{}: {}: {}",
        host.class,
        host.command.as_deref().unwrap_or("?"),
        tail(&attempt_record::sections_tail(receipt), 300)
    );
    if let Some(pool) = pool {
        let entry = st.env_pauses.entry(pool.clone()).or_default();
        entry.fails.push(now);
        entry
            .fails
            .retain(|at| now.saturating_sub(*at) <= envfail::HOST_FAIL_WINDOW);
        entry.evidence = evidence.clone();
        entry.probe = host
            .command
            .as_ref()
            .map(|tool| format!("command -v {tool}"));
        if entry.fails.len() >= envfail::HOST_PAUSE_AFTER && entry.paused_at.is_none() {
            entry.paused_at = Some(now);
            ctx.exception("environment", &pool, &evidence);
        }
    }
    ctx.log(&format!("host-fail {uid} {}; attempt restored", host.class));
}

/// Delete the unit's attempt branch, locally and on the remote, when either
/// has it: the branch only holds a `WIP (attempt failed)` marker, and the
/// restored attempt must not be mistaken for a delivered one. Failures are
/// logged and never fatal.
fn drop_attempt_branch(ctx: &Ctx, st: &State, uid: &str) {
    let Some(unit) = st.units.get(uid) else {
        return;
    };
    let Some(branch) = unit.branch.clone() else {
        return;
    };
    let target = unit.target_id().to_string();
    let tctx = ctx.for_target(&target).unwrap_or_else(|| ctx.clone());
    if local_branch_exists(&tctx, &branch) {
        let out = tctx.git(&format!("branch -D {}", shell_quote(&branch)), 60);
        ctx.log(&format!(
            "host-fail {uid} drop local branch {}: {}",
            branch,
            out.text.trim()
        ));
    }
    if remote_has(&tctx, &branch) {
        let out = tctx.git(
            &format!(
                "push {} --delete {}",
                shell_quote(&tctx.cfg.remote),
                shell_quote(&branch)
            ),
            60,
        );
        ctx.log(&format!(
            "host-fail {uid} drop remote branch {}: {}",
            branch,
            out.text.trim()
        ));
    }
}

/// Send a unit to the split planner: write the resplit record,
/// spawn the planner, and move the unit to `splitting` with a fresh clock.
/// A quarantine repair unit (one with a `base`) is never split: its work is
/// one branch, so a timeout counts as a failed attempt instead.
pub fn split(ctx: &Ctx, st: &mut State, uid: &str, text: &str) {
    if st.units.get(uid).is_some_and(|unit| unit.base.is_some()) {
        fail(ctx, st, uid, "timeout", &tail(text, 3000));
        return;
    }
    let now = ctx.now();
    let record = {
        let Some(unit) = st.units.get(uid) else {
            return;
        };
        ResplitRecord {
            id: uid.to_owned(),
            need: unit.need.clone(),
            target: unit.target_id().to_string(),
            title: unit.title.clone(),
            brief: unit.brief.clone(),
            write_scope: unit.write_scope.clone(),
            proof: unit.proof.clone(),
            tools: unit.tools.clone(),
            worker_said: tail(text, 3000),
        }
    };
    let dir = ctx.home.join("resplit");
    drop(fs::create_dir_all(&dir));
    drop(write_json(&dir.join(format!("{uid}.json")), &record));
    spawn_planner(ctx, "resplit", uid, &split_prompt(ctx, uid));
    let Some(unit) = st.units.get_mut(uid) else {
        return;
    };
    unit.status = Status::Splitting;
    unit.since = now;
    unit.history.push(Attempt {
        attempt: unit.attempts,
        model: unit.model.clone(),
        why: "timeout".to_owned(),
        detail: String::new(),
        at: now,
        branch: unit.branch.clone(),
        task: unit.task.clone(),
    });
    attempt_record::post(ctx, st, uid, &Ending::split(text));
    if let Some(unit) = st.units.get_mut(uid) {
        unit.task = None;
    }
    ctx.log(&format!("split-requested {uid}"));
}

/// Re-ask the split planner once after it wrote a bad split plan: the plan
/// file moves to `plans/rejected/`, its `plans_seen` key is forgotten, and
/// the planner respawns with the validation error appended to the prompt.
/// False when no re-ask applies — an unknown unit, a unit that is no longer
/// splitting or stuck, or a unit whose one re-ask is already spent.
pub(crate) fn resplit_bad_plan(ctx: &Ctx, st: &mut State, uid: &str, reason: &str) -> bool {
    let applies = st.units.get(uid).is_some_and(|unit| {
        matches!(unit.status, Status::Splitting | Status::Stuck) && unit.split_reasks == 0
    });
    if !applies {
        return false;
    }
    let now = ctx.now();
    let key = format!("{uid}.split.json");
    let rejected = ctx.home.join("plans/rejected");
    drop(fs::create_dir_all(&rejected));
    drop(fs::rename(ctx.home.join("plans").join(&key), rejected.join(format!("{key}.{now}"))));
    st.plans_seen.retain(|seen| seen != &key);
    let prompt = format!(
        "{}\nYour previous plan was rejected: {reason}. Fix that and write the complete plan again.\n",
        split_prompt(ctx, uid)
    );
    spawn_planner(ctx, "resplit", uid, &prompt);
    if let Some(unit) = st.units.get_mut(uid) {
        unit.split_reasks = 1;
        unit.split_retry_at = None;
        unit.split_fails = 0;
        unit.since = now;
    }
    ctx.log(&format!("split-reask {uid} {}", tail(reason, 120)));
    true
}

/// Reconcile the pool against in-flight units and the gate task.
/// Returns the gate task's receipt when the gate finished
/// (reported so, or unreported for more than 2400 s since it started); the
/// caller passes that receipt to the gate module.
pub fn collect(ctx: &Ctx, st: &mut State) -> Option<String> {
    let inflight: BTreeMap<String, String> = st
        .units
        .iter()
        .filter(|(_, unit)| unit.status == Status::Inflight)
        .filter_map(|(uid, unit)| unit.task.as_ref().map(|task| (task.clone(), uid.clone())))
        .collect();
    let gate_task = st.last_gate.task.clone();
    let out = if inflight.is_empty() && gate_task.is_none() {
        String::new()
    } else {
        ctx.vivi("pool reconcile --porcelain", 300).text
    };
    let seen = parse_reconcile(&out);
    for (task, uid) in &inflight {
        collect_one(ctx, st, &seen, task, uid);
    }
    expire_splits(ctx, st);
    gate_receipt(ctx, st, &seen, gate_task.as_deref())
}

/// Recover a worker result pushed before its pump state was saved.
///
/// # Errors
/// Returns an error when the configured remote cannot be queried.
pub fn recover_pushed_pending(ctx: &Ctx, st: &mut State) -> Result<(), VivariumError> {
    let mut pending: BTreeMap<String, Vec<(String, u32)>> = BTreeMap::new();
    for (uid, unit) in &st.units {
        if unit.status == Status::Pending {
            pending
                .entry(unit.target_id().to_string())
                .or_default()
                .push((uid.clone(), unit.attempts.saturating_add(1)));
        }
    }
    for (target, units) in pending {
        recover_target_branches(ctx, st, &target, &units)?;
    }
    Ok(())
}

fn recover_target_branches(
    ctx: &Ctx,
    st: &mut State,
    target: &str,
    units: &[(String, u32)],
) -> Result<(), VivariumError> {
    let target_ctx = ctx.for_target(target).ok_or_else(|| {
        VivariumError::Config(format!(
            "cannot recover branches for unknown target {target:?}"
        ))
    })?;
    let command = format!(
        "ls-remote --heads {} {}",
        shell_quote(&target_ctx.cfg.remote),
        shell_quote("refs/heads/unit/*")
    );
    let output = target_ctx.git(&command, 60);
    if output.code != 0 {
        return Err(VivariumError::Message(format!(
            "could not recover pushed unit branches for target {target}: {}",
            output.text.trim()
        )));
    }
    let branches = remote_unit_branches(&output.text);
    for (uid, attempt) in units {
        recover_one_pushed_branch(ctx, st, uid, *attempt, &branches);
    }
    Ok(())
}

fn remote_unit_branches(output: &str) -> BTreeSet<String> {
    output
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter_map(|reference| reference.strip_prefix("refs/heads/"))
        .filter(|branch| branch.starts_with("unit/"))
        .map(str::to_owned)
        .collect()
}

fn recover_one_pushed_branch(
    ctx: &Ctx,
    st: &mut State,
    uid: &str,
    attempt: u32,
    branches: &BTreeSet<String>,
) {
    let branch = format!("unit/{uid}-a{attempt}");
    if !branches.contains(&branch) {
        return;
    }
    let Some(unit) = st.units.get_mut(uid) else {
        return;
    };
    if unit.status != Status::Pending || unit.attempts.saturating_add(1) != attempt {
        return;
    }
    unit.status = Status::Delivered;
    unit.attempts = attempt;
    unit.task = None;
    unit.branch = Some(branch.clone());
    unit.since = ctx.now();
    unit.since_dispatch = Some(ctx.now());
    ctx.log(&format!(
        "recovered pushed unit {uid} attempt={attempt} branch={branch}"
    ));
}

/// Classify one in-flight unit in a strict order.
fn collect_one(
    ctx: &Ctx,
    st: &mut State,
    seen: &BTreeMap<String, (String, String)>,
    task: &str,
    uid: &str,
) {
    let (state, detail) = seen
        .get(task)
        .map_or((None, None), |(s, d)| (Some(s.as_str()), Some(d.as_str())));
    let (since, branch, target, pool) = {
        let Some(unit) = st.units.get(uid) else {
            return;
        };
        (
            unit.since,
            unit.branch.clone().unwrap_or_default(),
            unit.target_id().to_string(),
            unit.pool.clone(),
        )
    };
    if state == Some("running") {
        if ctx.now().saturating_sub(since)
            > ctx.cfg.dispatch.budget + ctx.cfg.dispatch.overdue_grace
        {
            split(ctx, st, uid, "overdue: still running long past its budget");
        }
        return;
    }
    if state == Some("timeout") || detail.is_some_and(|d| d.starts_with("timeout")) {
        split(ctx, st, uid, &receipt_full(ctx, task));
        return;
    }
    if state.is_none() && ctx.now().saturating_sub(since) < RECONCILE_GRACE {
        return;
    }
    if state.is_none() && still_on_pool(ctx, task) {
        return;
    }
    let target_ctx = ctx.for_target(&target).unwrap_or_else(|| ctx.clone());
    // A local worktree pool commits its unit branch into the pool's own
    // repository and never pushes to the target remote, so only that
    // repository can answer for it.
    let source = pool.as_deref().and_then(|name| ctx.pool_sources.get(name));
    let reachable = source.map_or_else(
        || local_branch_exists(&target_ctx, &branch) || remote_has(&target_ctx, &branch),
        |source| pool_source_has(ctx, source, &branch),
    );
    if reachable {
        deliver(ctx, st, task, uid, &branch, state);
        return;
    }
    fail_from_receipt(ctx, st, task, uid, state, detail);
}

/// The delivered path: the worker's branch reached the remote, so the unit
/// is delivered — unless the receipt shows the agent was killed by its
/// budget, which means split, or exited nonzero, which makes the attempt a
/// failed one (its branch is kept and recorded, never merged).
fn deliver(ctx: &Ctx, st: &mut State, task: &str, uid: &str, branch: &str, state: Option<&str>) {
    let full = if state == Some("finished") {
        receipt_full(ctx, task)
    } else {
        String::new()
    };
    if is_timeout(&full) {
        split(ctx, st, uid, &full);
        return;
    }
    if let Some(code) = attempt_record::agent_exit(&full).filter(|code| *code != 0) {
        let why = format!("finished/agent exit {code}");
        if let Some(failure) = envfail::classify(&why, "", &full) {
            host_fail(ctx, st, uid, &failure, &full);
            return;
        }
        fail_with(ctx, st, uid, &why, &tail(&full, 3000), Some(&full));
        return;
    }
    let now = ctx.now();
    let (secs, model, pool) = {
        let Some(unit) = st.units.get_mut(uid) else {
            return;
        };
        let secs = unit
            .since_dispatch
            .map_or(-1, |at| i64::try_from(now.saturating_sub(at)).unwrap_or(-1));
        let model = unit.model.clone().unwrap_or_else(|| "None".to_owned());
        let pool = unit.pool.clone().unwrap_or_else(|| "?".to_owned());
        unit.status = Status::Delivered;
        unit.since = now;
        (secs, model, pool)
    };
    let receipt = (!full.is_empty()).then_some(full.as_str());
    attempt_record::post(ctx, st, uid, &Ending::delivered(receipt));
    ctx.log(&format!(
        "delivered {uid} {branch} secs={secs} model={model} pool={pool}"
    ));
}

/// No remote branch: split on exit 124 or 137, accept a no-defect verdict, else
/// fail the attempt.
fn fail_from_receipt(
    ctx: &Ctx,
    st: &mut State,
    task: &str,
    uid: &str,
    state: Option<&str>,
    detail: Option<&str>,
) {
    let full = receipt_full(ctx, task);
    let text = tail(&full, 6000);
    if is_timeout(&full) {
        split(ctx, st, uid, &full);
        return;
    }
    if text.contains(NO_DEFECT_MARKER) {
        let verdict = tail(&text, 1500);
        let exc = tail(&text, 400);
        if let Some(unit) = st.units.get_mut(uid) {
            unit.status = Status::NoDefect;
            unit.verdict_text = Some(verdict);
        }
        attempt_record::post(ctx, st, uid, &Ending::no_defect(&full));
        ctx.exception("no-defect", uid, &exc);
        crate::pump::quarantine::repair_no_defect(ctx, st, uid);
        return;
    }
    let why = [state, detail]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("/");
    if let Some(failure) = envfail::classify(&why, detail.unwrap_or(""), &full) {
        host_fail(ctx, st, uid, &failure, &full);
        return;
    }
    fail_with(ctx, st, uid, &why, &text, Some(&full));
}

/// The dead split planner's raw output (`resplit/<uid>.out`) when the host
/// or harness killed the planner before it could plan — for example a
/// 28-byte `bash: pi: command not found`. The output has no receipt sections,
/// so the whole text is scanned as detail. `None` means the planner is
/// silent or answered: today's timeout path still decides.
fn dead_planner_output(ctx: &Ctx, uid: &str) -> Option<String> {
    let path = ctx.home.join("resplit").join(format!("{uid}.out"));
    let Ok(text) = fs::read_to_string(path) else {
        return None;
    };
    envfail::classify("planner", &text, "").map(|_| text)
}

/// Expire splitting units: a planner that died is retried with backoff,
/// a silent one past the hard cap is parked.
fn expire_splits(ctx: &Ctx, st: &mut State) {
    let now = ctx.now();
    let splitting: Vec<String> = st
        .units
        .iter()
        .filter(|(_, unit)| unit.status == Status::Splitting)
        .map(|(id, _)| id.clone())
        .collect();
    for uid in splitting {
        if expire_one_split(ctx, st, &uid, now) {
            crate::pump::quarantine::repair_stuck(ctx, st, &uid);
        }
    }
}

/// Decide one splitting unit: a written plan belongs to ingest, an exit
/// receipt without a plan is a dead planner retried with backoff, a due
/// retry respawns the planner, a planner that died host-class returns its
/// unit to `pending`, and a silent one past the hard cap is parked. True
/// when the unit became `Stuck`.
fn expire_one_split(ctx: &Ctx, st: &mut State, uid: &str, now: u64) -> bool {
    if ctx.home.join(format!("plans/{uid}.split.json")).exists() {
        return false;
    }
    let receipt = exit_receipt_path(&ctx.home.join(format!("resplit/{uid}.out")));
    let Some(unit) = st.units.get_mut(uid) else {
        return false;
    };
    if receipt.exists() {
        let raw = fs::read_to_string(&receipt).unwrap_or_default();
        let code = raw.trim().parse::<i32>().unwrap_or(-1);
        drop(fs::remove_file(&receipt));
        let fails = unit.split_fails + 1;
        unit.split_fails = fails;
        if fails >= SPLIT_RETRY_MAX {
            let text = format!(
                "the split planner exited without a plan {SPLIT_RETRY_MAX} times \
                 (last exit {code})"
            );
            unit.status = Status::Stuck;
            ctx.exception("split-planner-failed", uid, &text);
            return true;
        }
        let backoff = (60u64 << (fails - 1)).min(900);
        unit.split_retry_at = Some(now + backoff);
        ctx.exception(
            "split-planner-failed",
            uid,
            &format!("planner exited {code} without a plan; retry {fails}/3 in {backoff}s"),
        );
        return false;
    }
    if retry_split(ctx, unit, uid, now) {
        return false;
    }
    if let Some(text) = dead_planner_output(ctx, uid) {
        return split_planner_died(ctx, unit, uid, now, &text);
    }
    if now.saturating_sub(unit.since) > ctx.cfg.planner.split_hard_cap {
        unit.status = Status::Stuck;
        let text = format!(
            "the split planner is still unreachable after {} minutes",
            ctx.cfg.planner.split_hard_cap / 60
        );
        ctx.exception("split-no-answer", uid, &text);
        return true;
    }
    false
}

/// A recorded split retry: wait out the backoff, then respawn the planner
/// (which drops the stale receipt) with a fresh clock. Returns true when the
/// caller should stop here — waiting units are never parked, even past the
/// hard cap.
fn retry_split(ctx: &Ctx, unit: &mut Unit, uid: &str, now: u64) -> bool {
    let Some(at) = unit.split_retry_at else {
        return false;
    };
    if now < at {
        return true;
    }
    unit.split_retry_at = None;
    unit.since = now;
    spawn_planner(ctx, "resplit", uid, &split_prompt(ctx, uid));
    ctx.log(&format!("split-retry {uid}"));
    true
}

/// The split planner died host-class: return its unit to `pending` so it is
/// redispatched, and report the evidence. Never stuck.
fn split_planner_died(ctx: &Ctx, unit: &mut Unit, uid: &str, now: u64, text: &str) -> bool {
    let evidence: String = text.trim().chars().take(200).collect();
    unit.status = Status::Pending;
    unit.task = None;
    unit.since = now;
    unit.since_dispatch = None;
    ctx.exception(
        "environment",
        uid,
        &format!("split planner died: {evidence}"),
    );
    ctx.log(&format!("split-planner-dead {uid}"));
    false
}

/// The gate task's receipt when the gate finished; `None`
/// leaves it running for the next tick.
fn gate_receipt(
    ctx: &Ctx,
    st: &State,
    seen: &BTreeMap<String, (String, String)>,
    gate_task: Option<&str>,
) -> Option<String> {
    let gate_task = gate_task?;
    let state = seen.get(gate_task).map(|(s, _)| s.as_str());
    let reported = matches!(state, Some("finished" | "timeout" | "lost" | "error"));
    let unreported_too_long =
        state.is_none() && ctx.now().saturating_sub(st.last_gate.at) > GATE_GRACE;
    (reported || unreported_too_long).then(|| receipt(ctx, gate_task))
}

/// True while the task still holds a pool slot (`" <task> "` appears in
/// `vivi pool status --porcelain`).
fn still_on_pool(ctx: &Ctx, task: &str) -> bool {
    let status = ctx.vivi("pool status --porcelain", 120);
    status.text.contains(&format!(" {task} "))
}

/// True when `git ls-remote` lists the branch on the configured remote.
#[must_use]
pub(crate) fn remote_has(ctx: &Ctx, branch: &str) -> bool {
    let out = ctx.git(
        &format!(
            "ls-remote {} refs/heads/{}",
            ctx.cfg.remote,
            shell_quote(branch)
        ),
        60,
    );
    out.code == 0 && !out.text.trim().is_empty()
}

/// True when `git ls-remote` lists the branch in a local worktree pool's
/// repository, where that pool commits unit branches instead of pushing.
fn pool_source_has(ctx: &Ctx, source: &Path, branch: &str) -> bool {
    let out = ctx.git(
        &format!(
            "ls-remote {} refs/heads/{}",
            shell_quote(&source.to_string_lossy()),
            shell_quote(branch)
        ),
        60,
    );
    out.code == 0 && !out.text.trim().is_empty()
}

/// A local worktree target commits its result branch directly into the
/// target integration clone instead of pushing it to the configured remote.
pub(crate) fn local_branch_exists(ctx: &Ctx, branch: &str) -> bool {
    if ctx.cfg.targets.is_empty() {
        return false;
    }
    let out = ctx.git(
        &format!(
            "show-ref --verify --quiet refs/heads/{}",
            shell_quote(branch)
        ),
        30,
    );
    out.code == 0
}

/// True when the text marks the agent as killed by its budget timeout.
#[must_use]
fn is_timeout(text: &str) -> bool {
    matches!(attempt_record::agent_exit(text), Some(124 | 137))
        || text.lines().any(|line| {
            line.strip_prefix("agent_rc ")
                .and_then(|rest| rest.split_whitespace().next())
                .is_some_and(|code| matches!(code, "124" | "137"))
        })
}

/// `SPLIT_PROMPT` with the pump home and the unit handle substituted.
#[must_use]
fn split_prompt(ctx: &Ctx, uid: &str) -> String {
    SPLIT_PROMPT
        .replace("{home}", &ctx.home.to_string_lossy())
        .replace("{uid}", uid)
}

/// The last `max` characters of `text` (Python's `text[-max:]`).
#[must_use]
pub(crate) fn tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        text.to_owned()
    } else {
        text.chars().skip(count - max).collect()
    }
}

/// Write `value` as JSON with a one-space indent, like
/// `json.dump(..., indent=1)`.
fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let file = fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut writer, formatter);
    value.serialize(&mut serializer)?;
    writer.flush()
}
