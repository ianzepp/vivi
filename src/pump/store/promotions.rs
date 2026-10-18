//! The `promotions` table: promoted tips and the failing set main carried.

use rusqlite::{Connection, Transaction, params};

use crate::error::VivariumError;
use crate::pump::state::Promotion;

/// Add the `failing_json` column to a `promotions` table made before main's
/// failing set was recorded. Keyed off the table's columns, not the schema
/// version, so it is a no-op on a fresh table and safe to run on every open.
pub(super) fn ensure_failing_column(conn: &Connection) -> Result<(), VivariumError> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(promotions)")
        .map_err(db_err("read promotions columns"))?;
    let has = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(db_err("read promotions columns"))?
        .filter_map(Result::ok)
        .any(|name| name == "failing_json");
    drop(stmt);
    if !has {
        conn.execute("ALTER TABLE promotions ADD COLUMN failing_json TEXT", [])
            .map_err(db_err("add promotions.failing_json"))?;
    }
    Ok(())
}

/// Write every promotion (replacing a row with the same tip).
pub(super) fn write(tx: &Transaction<'_>, promotions: &[Promotion]) -> Result<(), VivariumError> {
    for promotion in promotions {
        let failing = match &promotion.failing {
            Some(names) => Some(
                serde_json::to_string(names)
                    .map_err(|err| VivariumError::Message(err.to_string()))?,
            ),
            None => None,
        };
        tx.execute(
            "INSERT OR REPLACE INTO promotions (tip, at, failing_json) VALUES (?1, ?2, ?3)",
            params![
                promotion.tip,
                i64::try_from(promotion.at).unwrap_or(0),
                failing
            ],
        )
        .map_err(db_err("write promotion"))?;
    }
    Ok(())
}

/// Read every promotion, oldest first.
pub(super) fn load(conn: &Connection) -> Result<Vec<Promotion>, VivariumError> {
    let mut stmt = conn
        .prepare("SELECT tip, at, failing_json FROM promotions ORDER BY at")
        .map_err(db_err("read promotions"))?;
    let rows = stmt
        .query_map([], |row| {
            let failing: Option<String> = row.get(2)?;
            Ok(Promotion {
                tip: row.get(0)?,
                at: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                failing: failing.and_then(|text| match serde_json::from_str(&text) {
                    Ok(names) => Some(names),
                    Err(_) => None,
                }),
            })
        })
        .map_err(db_err("read promotions"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read promotions"))
}

fn db_err<E: std::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> VivariumError {
    move |err| VivariumError::Other(format!("pump.db {what}: {err}"))
}
