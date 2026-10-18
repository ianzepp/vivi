//! Dispatch reconcile parsing and outcome classification.
//!
//! `collect` reconciles the pool once per tick, classifies every
//! in-flight unit (deliver, fail, split, no-defect), expires unanswered split
//! planners, and returns the gate task's receipt when the gate finished; the
//! caller hands that receipt to the gate module.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::pump::host::{Ctx, shell_quote, shell_split};
use crate::pump::state::{Attempt, State, Status};

/// Seconds a unit may stay unreported by reconcile before the pool-status
/// fallback decides.
const RECONCILE_GRACE: u64 = 90;
/// Receipt marker of a worker that found nothing to fix.
const NO_DEFECT_MARKER: &str = "VERDICT: no-defect";
/// Receipt fragments of an agent killed by its budget timeout.
const EXIT_124_MARKERS: [&str; 2] = ["agent exit 124", "agent_rc 124"];
/// Seconds a gate task may stay unreported before it is finished from its
/// receipt.
const GATE_GRACE: u64 = 2400;

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
    let out = ctx.vivi(&format!("task show {task} --latest"), 60);
    tail(&out.text, 6000)
}

/// Write the planner prompt to `<home>/<dir>/<id>.prompt` and start the
/// configured planner detached: stdin is the prompt file, output lands in
/// `<home>/<dir>/<id>.out`, the working directory is the pump home
/// Used by `split`, the board and gate fixes.
pub fn spawn_planner(ctx: &Ctx, dir: &str, id: &str, prompt: &str) {
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
    ctx.host
        .spawn(&command, Some(&ctx.home), Some(&prompt_path), &out);
}

/// Record a failed attempt: append the history entry, clear the task, and
/// either return the unit to `pending` or park it as `stuck` with an
/// exception once `max_attempts` is reached.
pub fn fail(ctx: &Ctx, st: &mut State, uid: &str, why: &str, detail: &str) {
    let now = ctx.now();
    let max = ctx.cfg.dispatch.max_attempts;
    let Some(unit) = st.units.get_mut(uid) else {
        return;
    };
    unit.history.push(Attempt {
        attempt: unit.attempts,
        model: unit.model.clone(),
        why: why.to_owned(),
        detail: tail(detail, 2500),
        at: now,
    });
    unit.task = None;
    let attempts = unit.attempts;
    let stuck = attempts >= max;
    unit.status = if stuck {
        Status::Stuck
    } else {
        Status::Pending
    };
    if stuck {
        ctx.exception("stuck", uid, &format!("{why}: {}", tail(detail, 300)));
    }
    ctx.log(&format!("fail {uid} attempt={attempts} {why}"));
}

/// Send a unit to the split planner: write the resplit record,
/// spawn the planner, and move the unit to `splitting` with a fresh clock.
pub fn split(ctx: &Ctx, st: &mut State, uid: &str, text: &str) {
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
    unit.task = None;
    unit.since = now;
    unit.history.push(Attempt {
        attempt: unit.attempts,
        model: unit.model.clone(),
        why: "timeout".to_owned(),
        detail: String::new(),
        at: now,
    });
    ctx.log(&format!("split-requested {uid}"));
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
    let (since, branch, target) = {
        let Some(unit) = st.units.get(uid) else {
            return;
        };
        (
            unit.since,
            unit.branch.clone().unwrap_or_default(),
            unit.target_id().to_string(),
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
        split(ctx, st, uid, &receipt(ctx, task));
        return;
    }
    if state.is_none() && ctx.now().saturating_sub(since) < RECONCILE_GRACE {
        return;
    }
    if state.is_none() && still_on_pool(ctx, task) {
        return;
    }
    let target_ctx = ctx.for_target(&target).unwrap_or_else(|| ctx.clone());
    if local_branch_exists(&target_ctx, &branch) || remote_has(&target_ctx, &branch) {
        deliver(ctx, st, task, uid, &branch, state);
        return;
    }
    fail_from_receipt(ctx, st, task, uid, state, detail);
}

/// The delivered path: the worker's branch reached the remote, so the unit
/// is delivered — unless the receipt shows the agent was killed by its
/// budget, which means split.
fn deliver(ctx: &Ctx, st: &mut State, task: &str, uid: &str, branch: &str, state: Option<&str>) {
    let text = if state == Some("finished") {
        receipt(ctx, task)
    } else {
        String::new()
    };
    if is_exit_124(&text) {
        split(ctx, st, uid, &text);
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
    ctx.log(&format!(
        "delivered {uid} {branch} secs={secs} model={model} pool={pool}"
    ));
}

/// No remote branch: split on exit 124, accept a no-defect verdict, else
/// fail the attempt.
fn fail_from_receipt(
    ctx: &Ctx,
    st: &mut State,
    task: &str,
    uid: &str,
    state: Option<&str>,
    detail: Option<&str>,
) {
    let text = receipt(ctx, task);
    if is_exit_124(&text) {
        split(ctx, st, uid, &text);
        return;
    }
    if text.contains(NO_DEFECT_MARKER) {
        let verdict = tail(&text, 1500);
        let exc = tail(&text, 400);
        if let Some(unit) = st.units.get_mut(uid) {
            unit.status = Status::NoDefect;
            unit.verdict_text = Some(verdict);
        }
        ctx.exception("no-defect", uid, &exc);
        return;
    }
    let why = format!("{}/{}", state.unwrap_or("None"), detail.unwrap_or("None"));
    fail(ctx, st, uid, &why, &text);
}

/// Park units whose split planner never answered.
fn expire_splits(ctx: &Ctx, st: &mut State) {
    let timeout = ctx.cfg.planner.split_timeout;
    let now = ctx.now();
    for (uid, unit) in &mut st.units {
        if unit.status == Status::Splitting && now.saturating_sub(unit.since) > timeout {
            unit.status = Status::Stuck;
            ctx.exception(
                "split-no-answer",
                uid,
                "the split planner wrote no plan in 25 minutes",
            );
        }
    }
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
fn remote_has(ctx: &Ctx, branch: &str) -> bool {
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

/// A local worktree target commits its result branch directly into the
/// target integration clone instead of pushing it to the configured remote.
fn local_branch_exists(ctx: &Ctx, branch: &str) -> bool {
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
fn is_exit_124(text: &str) -> bool {
    EXIT_124_MARKERS.iter().any(|marker| text.contains(marker))
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
fn tail(text: &str, max: usize) -> String {
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
