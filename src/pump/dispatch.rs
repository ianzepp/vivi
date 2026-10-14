//! Slots, task send and pool dispatch.
//!
//! Ports pump.py's `slots` (line 506) and `dispatch` (lines 576-616): count
//! free pool slots, offer ready units to the pools fix-units first, send each
//! unit's brief as a task, then hand the task to a pool with a model, branch
//! and budget.

use std::collections::BTreeMap;
use std::fs;

use crate::pump::brief::brief_for;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::ready::{is_ready, pick_model};
use crate::pump::state::{State, Status};

/// One unit's dispatch plan decided before anything is sent.
struct Plan {
    /// Pool the unit goes to (`pool_full` for `full` tools, else `pool_plain`).
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

/// Offer ready units to the pools: fix units first, at most `per_tick` starts
/// per tick. A unit with no free pool slot waits; a full lane set breaks the
/// loop for this tick. On success the unit goes inflight with its task,
/// branch, model, pool and dispatch timestamp; a failed pool dispatch closes
/// the task as superseded and leaves the unit pending.
pub fn dispatch(ctx: &Ctx, st: &mut State) {
    let mut free = slots(ctx);
    let mut started: u32 = 0;
    let mut order = st.order.clone();
    order.sort_by_key(|uid| !uid.starts_with(&ctx.cfg.fix_prefix));
    for uid in order {
        if started >= ctx.cfg.dispatch.per_tick {
            break;
        }
        let Some(plan) = plan_unit(ctx, st, &uid, &free) else {
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

/// Decide whether `uid` may start this tick and where: pending and ready,
/// its pool has a free slot, and a lane is open (reported separately, since a
/// closed lane set breaks the loop rather than skipping the unit).
fn plan_unit(ctx: &Ctx, st: &State, uid: &str, free: &BTreeMap<String, u32>) -> Option<Plan> {
    let unit = st.units.get(uid)?;
    if !is_ready(ctx, st, uid) {
        return None;
    }
    let pool = if unit.tools == "full" {
        ctx.cfg.dispatch.pool_full.clone()
    } else {
        ctx.cfg.dispatch.pool_plain.clone()
    };
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
    let cmd = format!(
        "pool dispatch --pool {} {} --model {} --from {} --branch {} --budget {} --porcelain",
        shell_quote(&plan.pool),
        shell_quote(task),
        shell_quote(model),
        shell_quote(ctx.cfg.dispatch_from()),
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
    ctx.vivi(&cmd, 120);
    if text.contains("no free slot") {
        free.insert(pool.to_owned(), 0);
    }
}
