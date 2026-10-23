//! The `need_branches` table of `pump.db`: one row per need naming its
//! integration branch, the base it was cut from and where it stands.
//!
//! `run`, `target` and `priors` (earlier branch names) live in the row's
//! `record_json` as one JSON object.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::VivariumError;
use crate::pump::flow::index::RunRow;

use super::{PumpStore, db_err};

/// One need's branch row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedBranchRow {
    pub need: String,
    pub branch: String,
    pub base: Option<String>,
    pub status: String,
    pub run: u32,
    pub target: String,
    /// Names of earlier branches of this need, kept for cleanup.
    pub priors: Vec<String>,
}

impl NeedBranchRow {
    /// Build a row for `run`: need and run come from the run row.
    #[must_use]
    pub fn for_run(
        run: &RunRow,
        branch: String,
        base: Option<String>,
        status: &str,
        target: &str,
        priors: Vec<String>,
    ) -> NeedBranchRow {
        NeedBranchRow {
            need: run.need.clone(),
            branch,
            base,
            status: status.to_string(),
            run: run.run,
            target: target.to_string(),
            priors,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Record {
    run: u32,
    target: String,
    #[serde(default)]
    priors: Vec<String>,
}

impl PumpStore {
    /// Read one need's branch row.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read or decoded.
    pub fn need_branch(&self, need: &str) -> Result<Option<NeedBranchRow>, VivariumError> {
        let raw = self
            .conn
            .query_row(
                "SELECT branch, base, status, record_json FROM need_branches WHERE need = ?1",
                [need],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(db_err("read need branch"))?;
        let Some((branch, base, status, record_json)) = raw else {
            return Ok(None);
        };
        let Some(text) = record_json else {
            return Err(VivariumError::Parse(format!(
                "need branch {need}: expected a record with run and target; actual: none"
            )));
        };
        let record: Record = serde_json::from_str(&text)
            .map_err(|err| VivariumError::Parse(format!("need branch {need}: {err}")))?;
        Ok(Some(NeedBranchRow {
            need: need.to_string(),
            branch,
            base,
            status,
            run: record.run,
            target: record.target,
            priors: record.priors,
        }))
    }

    /// Insert or replace one need's branch row.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn upsert_need_branch(&self, row: &NeedBranchRow) -> Result<(), VivariumError> {
        let record = serde_json::to_string(&Record {
            run: row.run,
            target: row.target.clone(),
            priors: row.priors.clone(),
        })
        .map_err(db_err("encode need branch record"))?;
        self.conn
            .execute(
                "INSERT INTO need_branches (need, branch, base, status, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(need) DO UPDATE SET
                     branch = ?2, base = ?3, status = ?4, record_json = ?5",
                params![row.need, row.branch, row.base, row.status, record],
            )
            .map_err(db_err("write need branch"))?;
        Ok(())
    }

    /// Change only the status of one need's branch row.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn set_need_branch_status(&self, need: &str, status: &str) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "UPDATE need_branches SET status = ?2 WHERE need = ?1",
                params![need, status],
            )
            .map_err(db_err("set need branch status"))?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "need_branch_test.rs"]
mod tests;
