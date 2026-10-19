//! Slots, task send and pool dispatch.
//!
//! Slot counting and dispatch: count
//! free pool slots, offer ready units to the pools fix-units first, send each
//! unit's brief as a task, then hand the task to a pool with a model, branch
//! and budget.

use std::collections::BTreeMap;
use std::fs;

use crate::pump::brief::brief_for;
use crate::pump::envfail;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::ready::{dependency_holds, is_ready, note_hold, pick_model, ranked};
use crate::pump::state::{State, Status, Unit};
use crate::routing::{choose_model, lanes_for};

/// The I/O outcome of trying to start one unit: pure command results with no
/// state applied, so [`start_unit`] can run off-thread and [`apply_start`]
/// can write the state later.
pub(crate) enum UnitStart {
    /// `pool dispatch` accepted the task.
    Started {
        /// The sent task's handle.
        task: String,
        /// The pool slot the porcelain reply named, when it named one.
        slot: Option<String>,
    },
    /// `task send` produced no handle (or the brief could not be written).
    SendFailed {
        /// The failure output, truncated for the log.
        text: String,
    },
    /// The task was sent, but `pool dispatch` failed.
    DispatchFailed {
        /// The sent task's handle.
        task: String,
        /// The full failure output.
        text: String,
    },
}

/// One unit's dispatch plan decided before anything is sent.
pub(crate) struct Plan {
    /// Pool chosen from the unit's kind (`[pump.dispatch.kinds.<name>] pools`).
    pub(crate) pool: String,
    /// Lane chosen by [`pick_model`]; `None` means every lane is full.
    pub(crate) model: Option<String>,
    /// One more than the unit's attempts so far.
    pub(crate) attempt: u32,
    /// `unit/<uid>-a<attempt>`.
    pub(crate) branch: String,
}

/// One unit reserved for a start: its id, a snapshot of the unit, the
/// dispatch plan decided for it, and the model picked for it. Phase one plans
/// and reserves; phase two starts the reservations one at a time.
pub(crate) struct Planned {
    /// The unit's handle.
    uid: String,
    /// The unit as it stood when it was planned.
    unit: Unit,
    /// The pool, lane, attempt and branch decided for the unit.
    plan: Plan,
    /// The model picked for the start.
    model: String,
}

/// Free slot counts per pool: run `pool status --porcelain` and count the
/// lines with at least five whitespace tokens whose first is `slot` and fifth
/// is `free`, keyed by the pool named in the second token.
#[must_use]
pub fn slots(ctx: &Ctx) -> BTreeMap<String, u32> {
    let out = ctx.vivi("pool status --porcelain", 120);
    let mut free = BTreeMap::new();
    for line in out.text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() >= 5 && tokens[0] == "slot" && tokens[4] == "free" {
            *free.entry(tokens[1].to_owned()).or_insert(0) += 1;
        }
    }
    free
}

/// The first `node`/`created` handle in `out`: a line whose trimmed form
/// starts with the keyword and whose last token is exactly 8 lowercase hex
/// characters, with at most one token between keyword and handle. Ports the
/// regex `^(?:node|created)\s+(?:\S+\s+)?([0-9a-f]{8})\s*$` in multiline mode.
#[must_use]
pub fn parse_created_handle(out: &str) -> Option<String> {
    for line in out.lines() {
        let trimmed = line.trim();
        let rest = match trimmed
            .strip_prefix("node")
            .or_else(|| trimmed.strip_prefix("created"))
        {
            Some(rest) if rest.is_empty() || rest.starts_with(char::is_whitespace) => rest,
            _ => continue,
        };
        let tokens: Vec<&str> = rest.split_whitespace().collect();
        let handle = match tokens.as_slice() {
            [handle] => *handle,
            [_between, handle] => *handle,
            _ => continue,
        };
        if is_hex8(handle) {
            return Some(handle.to_owned());
        }
    }
    None
}

/// `true` when `s` is exactly 8 lowercase hex characters.
fn is_hex8(s: &str) -> bool {
    s.len() == 8 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The last `n` characters of `s` (all of it when shorter).
fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        return s.to_owned();
    }
    s.chars().skip(count - n).collect()
}

/// Offer ready units to the pools: priority units first, then fix units, at most `per_tick` starts
/// per tick. A unit with no free pool slot waits; a full lane set breaks the
/// loop for this tick. Planning and slot reservation happen first and write no
/// unit state, so the same-tick overlay of earlier starts comes from the
/// planned slice; the starts themselves then run serially. On success the unit
/// goes inflight with its task, branch, model, pool and dispatch timestamp; a
/// failed pool dispatch closes the task as superseded and leaves the unit
/// pending. The per-unit I/O runs in [`start_unit`]; its result is applied to
/// the state by [`apply_start`]. The per-tick cap now counts planned starts,
/// so a failed send no longer falls through to the next unit within the same
/// tick; the next tick retries it.
pub fn dispatch(ctx: &Ctx, st: &mut State) {
    let mut free = slots(ctx);
    apply_env_pauses(ctx, st, &mut free);
    let holds = dependency_holds(ctx, st);
    let order = ranked(ctx, st, st.order.clone());
    let mut planned: Vec<Planned> = Vec::new();
    for uid in order {
        if planned.len() as u32 >= ctx.cfg.dispatch.per_tick {
            break;
        }
        let dependency = st
            .units
            .get(&uid)
            .and_then(|unit| holds.get(&unit.need))
            .and_then(|deps| deps.first())
            .cloned();
        if note_hold(ctx, st, &uid, Status::Pending, dependency) {
            continue;
        }
        if hold_unknown_target(ctx, st, &uid) {
            continue;
        }
        if report_unknown_kind(ctx, st, &uid) {
            continue;
        }
        let Some(plan) = plan_unit(ctx, st, &uid, &free, &holds, &planned) else {
            continue;
        };
        let Some(model) = plan.model.clone() else {
            if hand_pool_is_fixed(ctx) {
                continue;
            }
            break;
        };
        let Some(unit) = st.units.get(&uid) else {
            continue;
        };
        if let Some(count) = free.get_mut(&plan.pool) {
            *count = count.saturating_sub(1);
        }
        planned.push(Planned {
            uid: uid.clone(),
            unit: unit.clone(),
            plan,
            model,
        });
    }
    run_starts(ctx, st, &planned, &mut free);
}

/// `true` when the hand role names a pool, so a unit whose lanes are all full
/// waits for that pool instead of ending the tick.
fn hand_pool_is_fixed(ctx: &Ctx) -> bool {
    ctx.routing
        .hand(&ctx.cfg.to)
        .and_then(|role| role.pool.as_deref())
        .is_some()
}

/// Phase two of a dispatch tick: start the planned units one at a time and
/// apply each outcome to the state. Serial by design; [`start_unit`] owns the
/// I/O and [`apply_start`] owns every state write.
fn run_starts(ctx: &Ctx, st: &mut State, planned: &[Planned], free: &mut BTreeMap<String, u32>) {
    for p in planned {
        let start = start_unit(ctx, &p.uid, &p.unit, &p.plan, &p.model);
        apply_start(ctx, st, &p.uid, &p.plan, &p.model, start, free);
    }
}

/// Apply the pool environment pauses at the top of a dispatch tick: a pool
/// with a passing probe, or a probe-less pause older than
/// [`envfail::HOST_FAIL_WINDOW`], resumes (entry removed, pool's free slots
/// left in place); a still-paused pool keeps its entry and is dropped from
/// `free`, so dispatch finds no slot there. Pools absent from `free` need no
/// entry removal.
fn apply_env_pauses(ctx: &Ctx, st: &mut State, free: &mut BTreeMap<String, u32>) {
    for pool in st.env_pauses.keys().cloned().collect::<Vec<_>>() {
        let pause = &st.env_pauses[&pool];
        let paused = match &pause.probe {
            Some(probe) => ctx.sh(probe, None, 30).code != 0,
            None => ctx.now() - pause.fails.last().unwrap_or(&0) <= envfail::HOST_FAIL_WINDOW,
        };
        if paused {
            free.remove(&pool);
        } else {
            st.env_pauses.remove(&pool);
            ctx.log(&format!("environment cleared for pool {pool}"));
        }
    }
}

/// Park a unit whose target disappeared or no longer exists. Never let a
/// stale target silently fall back to the workspace's default checkout.
fn hold_unknown_target(ctx: &Ctx, st: &mut State, uid: &str) -> bool {
    let Some(unit) = st.units.get_mut(uid) else {
        return false;
    };
    if ctx.cfg.target_configs().contains_key(unit.target_id()) {
        if unit.status == Status::Held {
            unit.status = Status::Pending;
            ctx.log(&format!(
                "target restored for {uid}; returning unit to pending"
            ));
        }
        return false;
    }
    if unit.status == Status::Pending {
        unit.status = Status::Held;
        ctx.exception(
            "unknown-target",
            uid,
            &format!(
                "repository target `{}` is not configured; held before dispatch",
                unit.target_id()
            ),
        );
    }
    true
}

/// `true` when the unit names a kind that is not defined, so it cannot be
/// dispatched. A pending unit raises one `unknown-kind` exception, the first
/// time only (recorded on the unit, so a restart does not repeat it).
fn report_unknown_kind(ctx: &Ctx, st: &mut State, uid: &str) -> bool {
    let Some(unit) = st.units.get_mut(uid) else {
        return false;
    };
    if ctx.cfg.kind_of(&unit.tools).is_some() {
        return false;
    }
    if unit.status == Status::Pending && !unit.kind_reported {
        unit.kind_reported = true;
        let name = ctx.cfg.kind_name(&unit.tools);
        let text = if name.is_empty() {
            String::from("unit has no tools value and no default_kind is set; not dispatched")
        } else {
            format!(
                "unit names kind `{name}`, which is not defined under \
                 [pump.dispatch.kinds]; not dispatched"
            )
        };
        ctx.exception("unknown-kind", uid, &text);
    }
    true
}

/// Decide whether `uid` may start this tick and where: pending and ready,
/// under its kind's `max`, and one of the kind's pools has a free slot; a
/// closed lane set is reported separately, since it breaks the loop rather
/// than skipping the unit.
fn plan_unit(
    ctx: &Ctx,
    st: &State,
    uid: &str,
    free: &BTreeMap<String, u32>,
    holds: &BTreeMap<String, Vec<String>>,
    planned: &[Planned],
) -> Option<Plan> {
    let unit = st.units.get(uid)?;
    if !is_ready(ctx, st, uid, holds) {
        return None;
    }
    let kind = ctx.cfg.kind_of(&unit.tools)?;
    let kind_name = ctx.cfg.kind_name(&unit.tools);
    // While `max` units of this kind are inflight or planned this tick, a
    // further one waits.
    if let Some(max) = kind.max
        && inflight_of_kind(ctx, st, &kind_name)
            + planned
                .iter()
                .filter(|p| ctx.cfg.kind_name(&p.unit.tools) == kind_name)
                .count()
            >= max as usize
    {
        return None;
    }
    let pool = assigned_pool(ctx, &kind.pools, free)?;
    let attempt = unit.attempts + 1;
    Some(Plan {
        pool: pool.clone(),
        model: pick_unit_model(ctx, st, unit, &pool, planned),
        attempt,
        branch: format!("unit/{uid}-a{attempt}"),
    })
}

/// The role's pool when the hand role sets one (wait if it is full); else
/// the kind's pool with the most free slots.
fn assigned_pool(ctx: &Ctx, kind_pools: &[String], free: &BTreeMap<String, u32>) -> Option<String> {
    if let Some(pool) = ctx
        .routing
        .hand(&ctx.cfg.to)
        .and_then(|role| role.pool.clone())
    {
        return (free.get(&pool).copied().unwrap_or(0) >= 1).then_some(pool);
    }
    pick_pool(kind_pools, free)
}

fn pick_unit_model(
    ctx: &Ctx,
    st: &State,
    unit: &Unit,
    pool: &str,
    planned: &[Planned],
) -> Option<String> {
    let role = ctx.routing.hand(&ctx.cfg.to);
    if role.and_then(|role| role.pool.as_deref()) == Some(pool) {
        let lanes = lanes_for(&ctx.routing, &ctx.cfg, Some(pool));
        let mut load = inflight_models(st, Some(pool));
        for p in planned.iter().filter(|p| p.plan.pool == pool) {
            *load.entry(p.model.clone()).or_insert(0) += 1;
        }
        return choose_model(
            role,
            &lanes,
            &load,
            unit.attempts,
            unit.history.last().and_then(|h| h.model.as_deref()),
            ctx.cfg.dispatch.last_resort.as_str(),
            ctx.cfg.dispatch.max_attempts,
        );
    }
    pick_model(ctx, st, unit)
}

fn inflight_models(st: &State, pool: Option<&str>) -> BTreeMap<String, u32> {
    let mut load = BTreeMap::new();
    for other in st.units.values() {
        if other.status != Status::Inflight {
            continue;
        }
        if pool.is_some_and(|name| other.pool.as_deref() != Some(name)) {
            continue;
        }
        if let Some(model) = other.model.as_deref() {
            *load.entry(model.to_string()).or_insert(0) += 1;
        }
    }
    load
}

/// The pool with the most free slots among `pools` (ties: first listed);
/// `None` when none has a free slot.
fn pick_pool(pools: &[String], free: &BTreeMap<String, u32>) -> Option<String> {
    let mut best: Option<(&String, u32)> = None;
    for pool in pools {
        let n = free.get(pool).copied().unwrap_or(0);
        if n >= 1 && best.is_none_or(|(_, top)| n > top) {
            best = Some((pool, n));
        }
    }
    best.map(|(pool, _)| pool.clone())
}

/// Units with status `Inflight` whose `tools` select the kind `name`.
fn inflight_of_kind(ctx: &Ctx, st: &State, name: &str) -> usize {
    st.units
        .values()
        .filter(|u| u.status == Status::Inflight && ctx.cfg.kind_name(&u.tools) == name)
        .count()
}

/// Do the dispatch I/O for one planned unit without touching any state:
/// write `<home>/briefs/<uid>.txt`, `task send` the unit's task, then hand
/// the task to the pool with `pool dispatch`. The caller applies the outcome
/// with [`apply_start`], which owns every state write.
fn start_unit(
    ctx: &Ctx,
    uid: &str,
    unit: &Unit,
    plan: &Plan,
    model: &str,
) -> UnitStart {
    let dir = ctx.home.join("briefs");
    if let Err(err) = fs::create_dir_all(&dir) {
        ctx.log(&format!("brief dir failed {uid}: {err}"));
        return UnitStart::SendFailed {
            text: err.to_string(),
        };
    }
    let path = dir.join(format!("{uid}.txt"));
    if let Err(err) = fs::write(&path, brief_for(ctx, uid, unit)) {
        ctx.log(&format!("brief write failed {uid}: {err}"));
        return UnitStart::SendFailed {
            text: err.to_string(),
        };
    }
    let subject: String = format!("[{uid} a{}] {}", plan.attempt, unit.title)
        .chars()
        .take(170)
        .collect();
    let cmd = format!(
        "task send --from {} --to {} --subject {} --body-file {}",
        shell_quote(&ctx.cfg.from),
        shell_quote(&ctx.cfg.to),
        shell_quote(&subject),
        shell_quote(&path.to_string_lossy()),
    );
    let out = ctx.vivi(&cmd, 120);
    let Some(task) = parse_created_handle(&out.text) else {
        return UnitStart::SendFailed {
            text: tail(&out.text, 200),
        };
    };
    dispatch_cmd(ctx, unit, plan, model, &task)
}

/// Build and run the `pool dispatch` command for one sent task; the outcome
/// is returned for [`apply_start`] to act on.
fn dispatch_cmd(
    ctx: &Ctx,
    unit: &crate::pump::state::Unit,
    plan: &Plan,
    model: &str,
    task: &str,
) -> UnitStart {
    let cmd = format!(
        "pool dispatch --pool {} {} --target {} --model {} --from {} --branch {} --budget {} --porcelain",
        shell_quote(&plan.pool),
        shell_quote(task),
        shell_quote(unit.target_id()),
        shell_quote(model),
        shell_quote(&unit_base(ctx, unit)),
        shell_quote(&plan.branch),
        ctx.cfg.dispatch.budget,
    );
    let out = ctx.vivi(&cmd, 180);
    if out.code != 0 || !out.text.contains("dispatched") {
        return UnitStart::DispatchFailed {
            task: task.to_owned(),
            text: out.text,
        };
    }
    UnitStart::Started {
        task: task.to_owned(),
        slot: parse_dispatched_slot(&out.text),
    }
}

/// The slot from a `pool dispatch --porcelain` reply: the fourth token of the
/// line whose first token is `dispatched`
/// (`dispatched TASK POOL SLOT JOB MODEL FROM START BRANCH BUDGET MODE ABANDONED COST`),
/// with surrounding double quotes stripped. `-` (the porcelain's absent value)
/// and short lines yield `None`.
#[must_use]
pub fn parse_dispatched_slot(text: &str) -> Option<String> {
    let line = text
        .lines()
        .find(|line| line.split_whitespace().next() == Some("dispatched"))?;
    let slot = line.split_whitespace().nth(3)?;
    let slot = slot
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(slot);
    if slot.is_empty() || slot == "-" {
        return None;
    }
    Some(slot.to_owned())
}

/// Raise the unit's one `unknown-role` exception when a task send failed for
/// a config reason — the CLI refused a pump identity ("unknown local role
/// ..."), so retrying cannot help. Transient failures stay log-only. The
/// flag rides on the unit, so a restart does not repeat the exception; a
/// later successful send clears it (see [`apply_start`]).
fn report_send_failure(ctx: &Ctx, st: &mut State, uid: &str, text: &str) {
    if !text.contains("unknown local role") {
        return;
    }
    let Some(unit) = st.units.get_mut(uid) else {
        return;
    };
    if unit.send_reported {
        return;
    }
    unit.send_reported = true;
    ctx.exception(
        "unknown-role",
        uid,
        &format!("task send failed: {}", tail(text, 300)),
    );
}

/// The ref a unit is dispatched from: its own `base` when it has one (a
/// quarantine repair unit starts on its quarantine branch), else the target's
/// `dispatch.from`.
pub(crate) fn dispatch_base(ctx: &Ctx, st: &State, uid: &str) -> String {
    st.units.get(uid).map_or_else(
        || ctx.cfg.dispatch_from().to_string(),
        |unit| unit_base(ctx, unit),
    )
}

/// The dispatch base for one unit in hand: its own `base` when it has one,
/// else the target's `dispatch.from`.
fn unit_base(ctx: &Ctx, unit: &Unit) -> String {
    if let Some(base) = &unit.base {
        return base.clone();
    }
    ctx.cfg.for_target(unit.target_id()).map_or_else(
        || ctx.cfg.dispatch_from().to_string(),
        |cfg| cfg.dispatch_from().to_string(),
    )
}

/// Apply a [`UnitStart`] to the state — exactly the writes dispatching one
/// unit has always made, kept out of the I/O so [`start_unit`] can run off
/// thread. Returns `true` when the unit started.
pub(crate) fn apply_start(
    ctx: &Ctx,
    st: &mut State,
    uid: &str,
    plan: &Plan,
    model: &str,
    start: UnitStart,
    free: &mut BTreeMap<String, u32>,
) -> bool {
    match start {
        UnitStart::Started { task, slot } => apply_started(ctx, st, uid, plan, model, &task, slot),
        UnitStart::SendFailed { text } => {
            ctx.log(&format!("task send failed {uid}: {text}"));
            report_send_failure(ctx, st, uid, &text);
            // The slot was reserved at plan time and nothing started in it.
            release_slot(free, &plan.pool);
            false
        }
        UnitStart::DispatchFailed { task, text } => {
            // The send succeeded, so a raised send exception is cleared even
            // though the dispatch failed (send_brief always cleared it then).
            if let Some(unit) = st.units.get_mut(uid) {
                unit.send_reported = false;
            }
            dispatch_failed(ctx, uid, &task, &plan.pool, &text, free);
            false
        }
    }
}

/// Give a slot reserved at plan time back to its pool.
fn release_slot(free: &mut BTreeMap<String, u32>, pool: &str) {
    *free
        .entry(pool.to_owned())
        .and_modify(|c| *c = c.saturating_add(1))
        .or_insert(1);
}

/// The `Started` arm: the unit goes inflight with its task, branch, model,
/// pool and timestamps. The pool's slot was already reserved at plan time.
fn apply_started(
    ctx: &Ctx,
    st: &mut State,
    uid: &str,
    plan: &Plan,
    model: &str,
    task: &str,
    slot: Option<String>,
) -> bool {
    let Some(unit) = st.units.get_mut(uid) else {
        return false;
    };
    let now = ctx.now();
    unit.status = Status::Inflight;
    unit.attempts = plan.attempt;
    unit.task = Some(task.to_owned());
    unit.branch = Some(plan.branch.clone());
    unit.model = Some(model.to_owned());
    unit.since = now;
    unit.since_dispatch = Some(now);
    unit.pool = Some(plan.pool.clone());
    unit.slot = slot;
    unit.send_reported = false;
    ctx.log(&format!(
        "dispatch {uid} a{} {} {model} task={task}",
        plan.attempt, plan.pool
    ));
    true
}

/// Log a failed pool dispatch, close the task as superseded, and zero the
/// pool's free count when the failure text says `no free slot`; any other
/// failure gives the reserved slot back.
fn dispatch_failed(
    ctx: &Ctx,
    uid: &str,
    task: &str,
    pool: &str,
    text: &str,
    free: &mut BTreeMap<String, u32>,
) {
    let detail = tail(text, 300).replace('\n', " | ");
    ctx.log(&format!("dispatch failed {uid}: {detail}"));
    let cmd = format!(
        "task done {} --for {} --note 'dispatch failed; superseded'",
        shell_quote(task),
        shell_quote(&ctx.cfg.from),
    );
    drop(ctx.vivi(&cmd, 120));
    if text.contains("no free slot") {
        free.insert(pool.to_owned(), 0);
    } else {
        release_slot(free, pool);
    }
}
