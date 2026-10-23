//! The old pipeline `State` tables, written and read by `PumpStore::save` / `load`.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{db_err, from_json, from_u32, from_u64, json, json_or_null, unit_extra};
use crate::error::VivariumError;
use crate::pump::state::{Attempt, NeedRecord, State, Status, Unit};
pub(super) fn clear(tx: &Transaction<'_>) -> Result<(), VivariumError> {
    for table in [
        "needs",
        "plan_revisions",
        "units",
        "unit_edges",
        "unit_events",
        "unit_order",
        "merges",
        "closed_needs",
        "plans_seen",
        "meta",
    ] {
        tx.execute(&format!("DELETE FROM {table}"), [])
            .map_err(db_err("clear"))?;
    }
    Ok(())
}

pub(super) fn write_meta(tx: &Transaction<'_>, st: &State) -> Result<(), VivariumError> {
    put(tx, "initialized", "1")?;
    put(tx, "merges", &st.merges.to_string())?;
    put(tx, "board", &json_or_null(&st.board)?)?;
    put(tx, "extra", &json_or_null(&st.extra)?)?;
    put(tx, "rate_limited_at", &json_or_null(&st.rate_limited_at)?)?;
    put(tx, "jobs", &json_or_null(&st.jobs)?)?;
    Ok(())
}

pub(super) fn write_units(tx: &Transaction<'_>, st: &State) -> Result<(), VivariumError> {
    for (position, id) in st.order.iter().enumerate() {
        let position = i64::try_from(position).unwrap_or(0);
        tx.execute(
            "INSERT OR IGNORE INTO unit_order (position, unit) VALUES (?1, ?2)",
            params![position, id],
        )
        .map_err(db_err("write unit order"))?;
    }
    for (id, unit) in &st.units {
        write_unit(tx, id, unit)?;
    }
    Ok(())
}

fn write_unit(tx: &Transaction<'_>, id: &str, unit: &Unit) -> Result<(), VivariumError> {
    insert_unit_row(tx, id, unit)?;
    write_unit_edges(tx, id, unit)?;
    write_unit_events(tx, id, unit)?;
    write_merge_row(tx, id, unit)
}

fn insert_unit_row(tx: &Transaction<'_>, id: &str, unit: &Unit) -> Result<(), VivariumError> {
    tx.execute(
        "INSERT INTO units (id, need, target, title, brief, write_scope_json, after_json, proof_json,
            tools, status, attempts, task, branch, model, since, since_dispatch, pool,
            commit_sha, merged_at, outside_json, replaced_by_json, verdict_text,
            kind_reported, extra_json, revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
            ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
        params![
            id,
            &unit.need,
            &unit.target,
            &unit.title,
            &unit.brief,
            json(&unit.write_scope)?,
            json(&unit.after)?,
            json(&unit.proof)?,
            &unit.tools,
            unit.status.as_str(),
            i64::from(unit.attempts),
            &unit.task,
            &unit.branch,
            &unit.model,
            i64::try_from(unit.since).unwrap_or(0),
            unit.since_dispatch.map(|v| i64::try_from(v).unwrap_or(0)),
            &unit.pool,
            &unit.commit,
            unit.merged_at.map(|v| i64::try_from(v).unwrap_or(0)),
            json(&unit.outside)?,
            json(&unit.replaced_by)?,
            &unit.verdict_text,
            i64::from(unit.kind_reported),
            unit_extra::pack(unit)?,
            i64::try_from(unit.revision).unwrap_or(0),
        ],
    )
    .map_err(db_err("write unit"))?;
    Ok(())
}

fn write_unit_edges(tx: &Transaction<'_>, id: &str, unit: &Unit) -> Result<(), VivariumError> {
    for after in &unit.after {
        tx.execute(
            "INSERT OR IGNORE INTO unit_edges (unit, after_unit) VALUES (?1, ?2)",
            params![id, after],
        )
        .map_err(db_err("write unit edge"))?;
    }
    Ok(())
}

fn write_unit_events(tx: &Transaction<'_>, id: &str, unit: &Unit) -> Result<(), VivariumError> {
    for attempt in &unit.history {
        tx.execute(
            "INSERT INTO unit_events (unit, attempt, model, why, detail, at, branch, task)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                i64::from(attempt.attempt),
                &attempt.model,
                &attempt.why,
                &attempt.detail,
                i64::try_from(attempt.at).unwrap_or(0),
                &attempt.branch,
                &attempt.task,
            ],
        )
        .map_err(db_err("write unit event"))?;
    }
    Ok(())
}

fn write_merge_row(tx: &Transaction<'_>, id: &str, unit: &Unit) -> Result<(), VivariumError> {
    if let (Some(commit), Some(at)) = (unit.commit.as_deref(), unit.merged_at) {
        tx.execute(
            "INSERT INTO merges (unit, commit_sha, at, outside_json) VALUES (?1, ?2, ?3, ?4)",
            params![
                id,
                commit,
                i64::try_from(at).unwrap_or(0),
                json(&unit.outside)?
            ],
        )
        .map_err(db_err("write merge"))?;
    }
    Ok(())
}

pub(super) fn write_needs(tx: &Transaction<'_>, st: &State) -> Result<(), VivariumError> {
    for (handle, need) in &st.needs {
        tx.execute(
            "INSERT INTO needs (handle, verdict, reason, units_json, revision,
                thread_sha256, thread_seen_at, amendment_pending, withdrawn, extra_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                handle,
                &need.verdict,
                &need.reason,
                json(&need.units)?,
                i64::try_from(need.revision).unwrap_or(0),
                &need.thread_sha256,
                need.thread_seen_at.map(|at| i64::try_from(at).unwrap_or(0)),
                i64::from(need.amendment_pending),
                i64::from(need.withdrawn),
                json(&serde_json::json!({ "waits_on": &need.waits_on }))?,
            ],
        )
        .map_err(db_err("write need"))?;
    }
    Ok(())
}

pub(super) fn write_lists(tx: &Transaction<'_>, st: &State) -> Result<(), VivariumError> {
    for need in &st.closed_needs {
        tx.execute(
            "INSERT OR IGNORE INTO closed_needs (need) VALUES (?1)",
            params![need],
        )
        .map_err(db_err("write closed need"))?;
    }
    for name in &st.plans_seen {
        tx.execute(
            "INSERT OR IGNORE INTO plans_seen (name) VALUES (?1)",
            params![name],
        )
        .map_err(db_err("write plan seen"))?;
    }
    Ok(())
}

pub(super) fn load_meta(conn: &Connection, st: &mut State) -> Result<(), VivariumError> {
    st.merges = get(conn, "merges")?.map_or(0, |v| v.parse().unwrap_or(0));
    st.board = get_json(conn, "board")?;
    st.extra = get_json(conn, "extra")?;
    st.rate_limited_at = get_json(conn, "rate_limited_at")?;
    st.jobs = get_json(conn, "jobs")?;
    Ok(())
}

pub(super) fn load_units(conn: &Connection) -> Result<BTreeMap<String, Unit>, VivariumError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, need, target, title, brief, write_scope_json, after_json, proof_json,
                tools, status, attempts, task, branch, model, since, since_dispatch, pool,
                commit_sha, merged_at, outside_json, replaced_by_json, verdict_text,
                kind_reported, extra_json, revision
             FROM units",
        )
        .map_err(db_err("read units"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Unit {
                    need: row.get(1)?,
                    target: row.get(2)?,
                    title: row.get(3)?,
                    brief: row.get(4)?,
                    write_scope: from_json(row.get(5)?),
                    after: from_json(row.get(6)?),
                    proof: from_json(row.get(7)?),
                    tools: row.get(8)?,
                    status: Status::from_name(&row.get::<_, String>(9)?),
                    attempts: from_u32(row.get(10)?),
                    task: row.get(11)?,
                    branch: row.get(12)?,
                    model: row.get(13)?,
                    since: from_u64(row.get(14)?),
                    since_dispatch: row.get::<_, Option<i64>>(15)?.map(from_u64),
                    pool: row.get(16)?,
                    commit: row.get(17)?,
                    merged_at: row.get::<_, Option<i64>>(18)?.map(from_u64),
                    outside: from_json(row.get(19)?),
                    replaced_by: from_json(row.get(20)?),
                    verdict_text: row.get(21)?,
                    kind_reported: row.get::<_, i64>(22)? != 0,
                    held_on: None,
                    extra: from_json(row.get(23)?),
                    revision: from_u64(row.get(24)?),
                    history: Vec::new(),
                    ..Unit::default()
                },
            ))
        })
        .map_err(db_err("read units"))?;
    let mut units = BTreeMap::new();
    for row in rows {
        let (id, mut unit) = row.map_err(db_err("read unit"))?;
        unit_extra::lift(&mut unit);
        units.insert(id, unit);
    }
    load_events(conn, &mut units)?;
    Ok(units)
}

fn load_events(conn: &Connection, units: &mut BTreeMap<String, Unit>) -> Result<(), VivariumError> {
    let mut stmt = conn
        .prepare(
            "SELECT unit, attempt, model, why, detail, at, branch, task
             FROM unit_events ORDER BY event_id",
        )
        .map_err(db_err("read unit events"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Attempt {
                    attempt: from_u32(row.get(1)?),
                    model: row.get(2)?,
                    why: row.get(3)?,
                    detail: row.get(4)?,
                    at: from_u64(row.get(5)?),
                    branch: row.get(6)?,
                    task: row.get(7)?,
                },
            ))
        })
        .map_err(db_err("read unit events"))?;
    for row in rows {
        let (unit, attempt) = row.map_err(db_err("read unit event"))?;
        if let Some(unit) = units.get_mut(&unit) {
            unit.history.push(attempt);
        }
    }
    Ok(())
}

pub(super) fn load_needs(conn: &Connection) -> Result<BTreeMap<String, NeedRecord>, VivariumError> {
    let mut stmt = conn
        .prepare(
            "SELECT handle, verdict, reason, units_json, revision,
                thread_sha256, thread_seen_at, amendment_pending, withdrawn, extra_json
             FROM needs",
        )
        .map_err(db_err("read needs"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                NeedRecord {
                    verdict: row.get(1)?,
                    reason: row.get(2)?,
                    units: from_json(row.get(3)?),
                    revision: from_u64(row.get(4)?),
                    thread_sha256: row.get(5)?,
                    thread_seen_at: row.get::<_, Option<i64>>(6)?.map(from_u64),
                    amendment_pending: row.get::<_, i64>(7)? != 0,
                    withdrawn: row.get::<_, i64>(8)? != 0,
                    waits_on: waits_on_of(&row.get::<_, String>(9)?),
                },
            ))
        })
        .map_err(db_err("read needs"))?;
    let mut needs = BTreeMap::new();
    for row in rows {
        let (handle, need) = row.map_err(db_err("read need"))?;
        needs.insert(handle, need);
    }
    Ok(needs)
}

/// The `waits_on` list a need's `extra_json` carries; absent or unreadable
/// text means the need waits on nothing.
fn waits_on_of(extra: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(extra) else {
        return Vec::new();
    };
    match value.get("waits_on") {
        Some(list) => serde_json::from_value(list.clone()).unwrap_or_default(),
        None => Vec::new(),
    }
}

pub(super) fn load_order(conn: &Connection) -> Result<Vec<String>, VivariumError> {
    let mut stmt = conn
        .prepare("SELECT unit FROM unit_order ORDER BY position")
        .map_err(db_err("read unit order"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(db_err("read unit order"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read unit order"))
}

pub(super) fn load_strings(
    conn: &Connection,
    table: &str,
    column: &str,
) -> Result<Vec<String>, VivariumError> {
    let mut stmt = conn
        .prepare(&format!("SELECT {column} FROM {table}"))
        .map_err(db_err("read list"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(db_err("read list"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read list"))
}

fn put(tx: &Transaction<'_>, key: &str, value: &str) -> Result<(), VivariumError> {
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .map_err(db_err("write meta"))?;
    Ok(())
}

fn get(conn: &Connection, key: &str) -> Result<Option<String>, VivariumError> {
    conn.query_row(
        "SELECT value FROM meta WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .optional()
    .map_err(db_err("read meta"))
}

fn get_json<T: Default + serde::de::DeserializeOwned>(
    conn: &Connection,
    key: &str,
) -> Result<T, VivariumError> {
    match get(conn, key)? {
        Some(text) => serde_json::from_str(&text)
            .map_err(|err| VivariumError::Message(format!("pump.db meta {key}: {err}"))),
        None => Ok(T::default()),
    }
}
