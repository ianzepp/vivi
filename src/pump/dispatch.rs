//! Slots, task send and pool dispatch.
//!
//! Slot counting and dispatch: count
//! free pool slots, offer ready units to the pools fix-units first, send each
//! unit's brief as a task, then hand the task to a pool with a model, branch
//! and budget.

use std::collections::BTreeMap;
use std::fs;

use crate::pump::brief::brief_for;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::ready::{dependency_holds, is_ready, note_hold, pick_model, ranked};
use crate::pump::state::{State, Status};

/// One unit's dispatch plan decided before anything is sent.
struct Plan {
    /// Pool of the unit's kind (`[pump.dispatch.kinds.<name>] pool`).
    pool: String,
    /// Lane chosen by [`pick_model`]; `None` means every lane is full.
    model: Option<String>,
    /// One more than the unit's attempts so far.
    attempt: u32,
    /// `unit/<uid>-a<attempt>`.
    branch: String,
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
/// loop for this tick. On success the unit goes inflight with its task,
/// branch, model, pool and dispatch timestamp; a failed pool dispatch closes
/// the task as superseded and leaves the unit pending.
pub fn dispatch(ctx: &Ctx, st: &mut State) {
    let mut free = slots(ctx);
    let holds = dependency_holds(ctx, st);
    let mut started: u32 = 0;
    let order = ranked(ctx, st, st.order.clone());
    for uid in order {
        if started >= ctx.cfg.dispatch.per_tick {
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
        let Some(plan) = plan_unit(ctx, st, &uid, &free, &holds) else {
            continue;
        };
        let Some(model) = plan.model.clone() else {
            break;
        };
        let Some(task) = send_brief(ctx, st, &uid, plan.attempt) else {
            continue;
        };
        if dispatch_one(ctx, st, &uid, &plan, &model, &task, &mut free) {
            started += 1;
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
/// under its kind's `max`, and the kind's pool has a free slot; a
/// closed lane set is reported separately, since it breaks the loop rather
/// than skipping the unit.
fn plan_unit(
    ctx: &Ctx,
    st: &State,
    uid: &str,
    free: &BTreeMap<String, u32>,
    holds: &BTreeMap<String, Vec<String>>,
) -> Option<Plan> {
    let unit = st.units.get(uid)?;
    if !is_ready(ctx, st, uid, holds) {
        return None;
    }
    let kind = ctx.cfg.kind_of(&unit.tools)?;
    // While `max` units of this kind are inflight, a further one waits.
    if let Some(max) = kind.max
        && inflight_of_kind(ctx, st, ctx.cfg.kind_name(&unit.tools)) >= max as usize
    {
        return None;
    }
    let pool = kind.pool.clone();
    if free.get(&pool).copied().unwrap_or(0) < 1 {
        return None;
    }
    let attempt = unit.attempts + 1;
    Some(Plan {
        pool,
        model: pick_model(ctx, st, unit),
        attempt,
        branch: format!("unit/{uid}-a{attempt}"),
    })
}

/// Units with status `Inflight` whose `tools` select the kind `name`.
fn inflight_of_kind(ctx: &Ctx, st: &State, name: &str) -> usize {
    st.units
        .values()
        .filter(|u| u.status == Status::Inflight && ctx.cfg.kind_name(&u.tools) == name)
        .count()
}

/// Write `<home>/briefs/<uid>.txt` and send the unit's task; returns the
/// created task handle, or `None` when the brief could not be written or the
/// output carries no handle (logged as `task send failed`).
fn send_brief(ctx: &Ctx, st: &State, uid: &str, attempt: u32) -> Option<String> {
    let unit = st.units.get(uid)?;
    let dir = ctx.home.join("briefs");
    if let Err(err) = fs::create_dir_all(&dir) {
        ctx.log(&format!("brief dir failed {uid}: {err}"));
        return None;
    }
    let path = dir.join(format!("{uid}.txt"));
    if let Err(err) = fs::write(&path, brief_for(ctx, uid, unit)) {
        ctx.log(&format!("brief write failed {uid}: {err}"));
        return None;
    }
    let subject: String = format!("[{uid} a{attempt}] {}", unit.title)
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
        ctx.log(&format!("task send failed {uid}: {}", tail(&out.text, 200)));
        return None;
    };
    Some(task)
}

/// Run `pool dispatch` for one planned unit. On success the unit goes
/// inflight and a slot is consumed; on failure the task is closed as
/// superseded and, when the output says `no free slot`, the pool's free count
/// is zeroed. Returns `true` when the unit started.
fn dispatch_one(
    ctx: &Ctx,
    st: &mut State,
    uid: &str,
    plan: &Plan,
    model: &str,
    task: &str,
    free: &mut BTreeMap<String, u32>,
) -> bool {
    let target = st.units.get(uid).map_or("default", |unit| unit.target_id());
    let from = ctx.cfg.for_target(target).map_or_else(
        || ctx.cfg.dispatch_from().to_string(),
        |cfg| cfg.dispatch_from().to_string(),
    );
    let cmd = format!(
        "pool dispatch --pool {} {} --target {} --model {} --from {} --branch {} --budget {} --porcelain",
        shell_quote(&plan.pool),
        shell_quote(task),
        shell_quote(&st.units.get(uid).map_or("default", |unit| unit.target_id())),
        shell_quote(model),
        shell_quote(&from),
        shell_quote(&plan.branch),
        ctx.cfg.dispatch.budget,
    );
    let out = ctx.vivi(&cmd, 180);
    if out.code != 0 || !out.text.contains("dispatched") {
        dispatch_failed(ctx, uid, task, &plan.pool, &out.text, free);
        return false;
    }
    let now = ctx.now();
    let Some(unit) = st.units.get_mut(uid) else {
        return false;
    };
    unit.status = Status::Inflight;
    unit.attempts = plan.attempt;
    unit.task = Some(task.to_owned());
    unit.branch = Some(plan.branch.clone());
    unit.model = Some(model.to_owned());
    unit.since = now;
    unit.since_dispatch = Some(now);
    unit.pool = Some(plan.pool.clone());
    if let Some(count) = free.get_mut(&plan.pool) {
        *count = count.saturating_sub(1);
    }
    ctx.log(&format!(
        "dispatch {uid} a{} {} {model} task={task}",
        plan.attempt, plan.pool
    ));
    true
}

/// Log a failed pool dispatch, close the task as superseded, and zero the
/// pool's free count when the failure text says `no free slot`.
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
    }
}
