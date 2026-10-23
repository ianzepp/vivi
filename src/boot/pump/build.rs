//! Turn local pump sources into a `PumpSnapshot`.

use super::super::pump_slots::LocalSlotMark;
use super::gate::PumpView;
use super::{PumpExceptionRow, PumpPoolRow, PumpSnapshot, PumpSources, PumpUnitRow};
use crate::pool::config::PoolConfig;
use crate::pump::state::{State, Status, Unit};
use crate::routing::RoutedRole;

pub(super) fn snapshot(
    sources: &PumpSources<'_>,
    view: PumpView,
    role: Option<&str>,
    here: Option<&LocalSlotMark>,
) -> PumpSnapshot {
    let mut snap = full(sources);
    if view == PumpView::Own {
        snap = own(snap, role, here);
    }
    snap
}

fn full(sources: &PumpSources<'_>) -> PumpSnapshot {
    let units = sources.state.map(|state| rows(sources, state));
    let mut snap = PumpSnapshot {
        pools: pool_rows(sources),
        lanes: lanes(sources.status_txt),
        needs_ready: needs_ready(sources),
        exceptions: exception_rows(sources),
        ..PumpSnapshot::default()
    };
    if let Some(rows) = units {
        fill_units(&mut snap, &rows, sources.now);
    }
    snap
}

fn fill_units(snap: &mut PumpSnapshot, rows: &[PumpUnitRow], now: u64) {
    snap.oldest_wait = oldest_delivered(rows, now);
    snap.inflight = take(rows, |row| {
        row.status == "inflight" || row.status == "merging"
    });
    snap.merge_queue = take(rows, |row| {
        row.status == "delivered" || row.status == "merging"
    });
    snap.held = take(rows, |row| row.status == "held" || row.held_on.is_some());
    snap.stuck = take(rows, |row| row.status == "stuck");
}

fn own(mut snap: PumpSnapshot, role: Option<&str>, here: Option<&LocalSlotMark>) -> PumpSnapshot {
    snap.inflight.retain(|row| mine(row, role, here));
    snap.merge_queue.clear();
    snap.oldest_wait = None;
    snap.held.clear();
    snap.needs_ready.clear();
    snap.stuck.retain(|row| mine(row, role, here));
    snap.exceptions.clear();
    snap.lanes.clear();
    snap.pools.clear();
    snap
}

fn mine(row: &PumpUnitRow, role: Option<&str>, here: Option<&LocalSlotMark>) -> bool {
    if let Some(slot) = here {
        if row.slot.as_deref() == Some(slot.id.as_str()) {
            return true;
        }
        if slot.task.is_some() && row.task == slot.task {
            return true;
        }
    }
    let Some(role) = role else {
        return here.is_some();
    };
    row.role.as_deref() == Some(role) || row.holder.as_deref() == Some(role)
}

fn rows(sources: &PumpSources<'_>, state: &State) -> Vec<PumpUnitRow> {
    state
        .order
        .iter()
        .filter_map(|id| state.units.get(id).map(|unit| (id.as_str(), unit)))
        .chain(
            state
                .units
                .iter()
                .filter(|(id, _)| !state.order.iter().any(|known| known == *id))
                .map(|(id, unit)| (id.as_str(), unit)),
        )
        .map(|(id, unit)| unit_row(sources, state, id, unit))
        .collect()
}

fn unit_row(sources: &PumpSources<'_>, state: &State, id: &str, unit: &Unit) -> PumpUnitRow {
    let slot = match_slot(sources, unit);
    let role = unit_role(sources, unit);
    let remote = unit
        .pool
        .as_deref()
        .is_some_and(|pool| sources.pools.get(pool).is_some_and(|cfg| !is_local(cfg)));
    PumpUnitRow {
        unit: id.to_string(),
        need: unit.need.clone(),
        status: unit.status.as_str().to_string(),
        holder: unit
            .task
            .as_ref()
            .and_then(|task| sources.holders.get(task).cloned()),
        role,
        pool: unit.pool.clone(),
        slot: slot_label(slot, remote),
        task: unit.task.clone(),
        model: unit.model.clone(),
        branch: unit.branch.clone(),
        age: age_of(unit, sources.now),
        budget: budget_of(unit, sources),
        checkout: slot.and_then(|s| s.checkout.as_ref().map(|p| p.display().to_string())),
        held_on: held_on(state, unit),
        after: unit.after.clone(),
        base_ref: slot.and_then(|s| s.base_ref.clone()),
        base_sha: slot.and_then(|s| s.base_sha.clone()),
    }
}

fn unit_role(sources: &PumpSources<'_>, unit: &Unit) -> Option<String> {
    if let Some(task) = &unit.task
        && let Some(role) = sources.holders.get(task)
    {
        return Some(role.clone());
    }
    pool_role(sources, unit.pool.as_deref()).or_else(|| {
        sources
            .routing
            .lookup(sources.pump_to)
            .map(|r| r.name.clone())
    })
}

fn pool_role(sources: &PumpSources<'_>, pool: Option<&str>) -> Option<String> {
    let pool = pool?;
    let assigned: Vec<&RoutedRole> = sources
        .routing
        .roles
        .iter()
        .filter(|role| role.pool.as_deref() == Some(pool))
        .collect();
    if assigned.len() == 1 {
        return Some(assigned[0].name.clone());
    }
    assigned
        .iter()
        .find(|role| kind_is(role, "hand"))
        .map(|role| role.name.clone())
}

fn kind_is(role: &RoutedRole, kind: &str) -> bool {
    role.kind
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case(kind))
}

fn match_slot<'a>(sources: &'a PumpSources<'_>, unit: &Unit) -> Option<&'a LocalSlotMark> {
    let task = unit.task.as_deref()?;
    sources
        .slots
        .iter()
        .find(|slot| slot.task.as_deref() == Some(task))
}

fn slot_label(slot: Option<&LocalSlotMark>, remote: bool) -> Option<String> {
    if let Some(slot) = slot {
        return Some(slot.id.clone());
    }
    remote.then(|| String::from("remote"))
}

fn held_on(state: &State, unit: &Unit) -> Option<String> {
    if let Some(held) = &unit.held_on {
        return Some(held.clone());
    }
    unit.after.iter().find_map(|dep| {
        let landed = state
            .units
            .get(dep)
            .is_some_and(|other| matches!(other.status, Status::Merged | Status::NoDefect));
        (!landed).then(|| dep.clone())
    })
}

fn age_of(unit: &Unit, now: u64) -> Option<String> {
    let start = unit.since_dispatch.unwrap_or(unit.since);
    (start > 0).then(|| age_label(now.saturating_sub(start)))
}

fn budget_of(unit: &Unit, sources: &PumpSources<'_>) -> Option<String> {
    if !matches!(unit.status, Status::Inflight) {
        return None;
    }
    Some(age_label(sources.budget_secs))
}

fn oldest_delivered(rows: &[PumpUnitRow], _now: u64) -> Option<String> {
    rows.iter()
        .filter(|row| row.status == "delivered")
        .filter_map(|row| row.age.as_deref())
        .max_by_key(|age| age_rank(age))
        .map(str::to_string)
}

fn age_rank(age: &str) -> u64 {
    let (num, rest) = age.split_at(age.find(|c: char| !c.is_ascii_digit()).unwrap_or(age.len()));
    let n: u64 = num.parse().unwrap_or(0);
    match rest {
        "d" => n.saturating_mul(86_400),
        "h" => n.saturating_mul(3_600),
        "m" => n.saturating_mul(60),
        _ => n,
    }
}

fn take(rows: &[PumpUnitRow], keep: impl Fn(&PumpUnitRow) -> bool) -> Vec<PumpUnitRow> {
    rows.iter().filter(|row| keep(row)).cloned().collect()
}

fn needs_ready(sources: &PumpSources<'_>) -> Vec<String> {
    let mut ready: Vec<String> = sources
        .complete_needs
        .intersection(sources.open_needs)
        .cloned()
        .collect();
    if let Some(state) = sources.state {
        for need in &state.closed_needs {
            if !ready.iter().any(|known| known == need) {
                ready.push(need.clone());
            }
        }
    }
    ready.sort();
    ready
}

fn exception_rows(sources: &PumpSources<'_>) -> Vec<PumpExceptionRow> {
    sources
        .exception_rows
        .iter()
        .map(|(at, kind, unit, text)| PumpExceptionRow {
            kind: kind.clone(),
            unit: unit.clone(),
            text: text.clone(),
            age: (*at > 0).then(|| age_label(sources.now.saturating_sub(*at))),
        })
        .collect()
}

fn lanes(status: Option<&str>) -> Vec<String> {
    status
        .unwrap_or("")
        .lines()
        .filter(|line| line.starts_with("lane "))
        .map(str::to_string)
        .collect()
}

fn pool_rows(sources: &PumpSources<'_>) -> Vec<PumpPoolRow> {
    sources
        .pools
        .iter()
        .map(|(name, pool)| pool_row(sources, name, pool))
        .collect()
}

fn pool_row(sources: &PumpSources<'_>, name: &str, pool: &PoolConfig) -> PumpPoolRow {
    let local = is_local(pool);
    let used = if local {
        sources
            .slots
            .iter()
            .filter(|slot| slot.pool == name && slot.task.is_some())
            .count()
    } else {
        remote_used(sources.state, name)
    };
    PumpPoolRow {
        name: name.to_string(),
        provider: pool.provider.clone(),
        used,
        total: pool.slots,
        root: local.then(|| pool.local.root_or_default().to_string()),
        worktree_of: pool.local.worktree_of.clone(),
        build_dir: pool.build_dir.clone(),
        roles: roles_for(sources, name),
        remote: !local,
    }
}

fn remote_used(state: Option<&State>, pool: &str) -> usize {
    let Some(state) = state else {
        return 0;
    };
    state
        .units
        .values()
        .filter(|unit| {
            unit.pool.as_deref() == Some(pool)
                && matches!(unit.status, Status::Inflight | Status::Merging)
        })
        .count()
}

fn roles_for(sources: &PumpSources<'_>, pool: &str) -> Vec<String> {
    sources
        .routing
        .roles
        .iter()
        .filter(|role| role.pool.as_deref() == Some(pool))
        .map(|role| role.name.clone())
        .collect()
}

fn is_local(pool: &PoolConfig) -> bool {
    pool.provider.eq_ignore_ascii_case("local")
}

pub(super) fn age_label(seconds: u64) -> String {
    if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}
