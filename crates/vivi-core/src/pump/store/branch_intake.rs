//! Branch-intake tables of `pump.db`.
//!
//! The tables are additive and independent of the `State` save: `save()` and
//! `clear()` never touch them, and the schema version is not bumped. A
//! read-only database written before these tables existed reads as no
//! history; a partially present, malformed, or corrupt intake schema is an
//! error.

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{PumpStore, db_err};
use crate::pump::branch_intake::model::{
    BranchState, GenerationRecord, GenerationStage, IntakeIntent, IntakeIntentKind, ProofPurpose,
    ProofRecord, TipRecord,
};
use vivi_error::VivariumError;

pub mod lifecycle;

const TABLES: [&str; 5] = [
    "branch_intake_state",
    "branch_intake_tips",
    "branch_intake_generations",
    "branch_intake_proofs",
    "branch_intake_intents",
];

const INTAKE_DDL: &str = "
CREATE TABLE IF NOT EXISTS branch_intake_state (
    target TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    latest_observation_json TEXT,
    active_generation INTEGER,
    notification_json TEXT NOT NULL,
    publication_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (target, source_ref)
);
CREATE TABLE IF NOT EXISTS branch_intake_tips (
    target TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    source_sha TEXT NOT NULL,
    observed_main TEXT NOT NULL,
    observed_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (target, source_ref, source_sha)
);
CREATE TABLE IF NOT EXISTS branch_intake_generations (
    target TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    generation INTEGER NOT NULL,
    observed_main TEXT NOT NULL,
    candidate_sha TEXT NOT NULL,
    candidate_ref TEXT NOT NULL,
    construction_author TEXT NOT NULL,
    construction_committer TEXT NOT NULL,
    construction_message TEXT NOT NULL,
    constructed_at INTEGER NOT NULL DEFAULT 0,
    stage TEXT NOT NULL,
    reasons_json TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (target, source_ref, generation)
);
CREATE TABLE IF NOT EXISTS branch_intake_proofs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    purpose TEXT NOT NULL,
    target TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    generation INTEGER NOT NULL,
    tip TEXT NOT NULL,
    rung TEXT NOT NULL,
    command TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    job TEXT NOT NULL,
    task TEXT NOT NULL,
    inputs TEXT NOT NULL,
    results TEXT NOT NULL,
    evidence_json TEXT NOT NULL,
    lines_json TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS branch_intake_proofs_generation
    ON branch_intake_proofs (target, source_ref, generation);
CREATE TABLE IF NOT EXISTS branch_intake_intents (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    target TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    generation INTEGER,
    candidate_ref TEXT,
    candidate_sha TEXT,
    body TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT 0,
    delivered_at INTEGER
);
CREATE INDEX IF NOT EXISTS branch_intake_intents_branch
    ON branch_intake_intents (target, source_ref, id);
";

/// Create the branch-intake tables when they are missing.
pub(super) fn ensure_tables(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(INTAKE_DDL)
        .map_err(db_err("create branch intake tables"))
}

/// True when every intake table exists; `None` when none exists at all.
fn schema_presence(conn: &Connection) -> Result<Option<bool>, VivariumError> {
    let mut present = 0;
    for table in TABLES {
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .map_err(db_err("read branch intake schema"))?;
        if exists > 0 {
            present += 1;
        }
    }
    match present {
        0 => Ok(None),
        n if n == TABLES.len() => Ok(Some(true)),
        n => Err(VivariumError::Parse(format!(
            "pump.db branch intake: partial schema, {n} of {} tables present",
            TABLES.len()
        ))),
    }
}

fn present(conn: &Connection) -> Result<bool, VivariumError> {
    Ok(schema_presence(conn)?.unwrap_or(false))
}

fn decode_stage(text: &str) -> Result<GenerationStage, VivariumError> {
    GenerationStage::parse(text)
}

fn decode_json<T: serde::de::DeserializeOwned>(
    what: &'static str,
    text: String,
) -> Result<T, VivariumError> {
    serde_json::from_str(&text)
        .map_err(|err| VivariumError::Parse(format!("pump.db branch intake {what}: {err}")))
}

fn encode_json<T: serde::Serialize>(
    what: &'static str,
    value: &T,
) -> Result<String, VivariumError> {
    serde_json::to_string(value)
        .map_err(|err| VivariumError::Parse(format!("pump.db branch intake {what}: {err}")))
}

type GenerationRaw = (
    String,
    String,
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
    String,
    i64,
    i64,
);

fn generation_from_row(row: &Row<'_>) -> rusqlite::Result<GenerationRaw> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
    ))
}

fn decode_generation(raw: GenerationRaw) -> Result<GenerationRecord, VivariumError> {
    let (
        target,
        source_ref,
        generation,
        observed_main,
        candidate_sha,
        candidate_ref,
        construction_author,
        construction_committer,
        construction_message,
        constructed_at,
        stage,
        reasons,
        created_at,
        updated_at,
    ) = raw;
    Ok(GenerationRecord {
        target,
        source_ref,
        generation: u64::try_from(generation).unwrap_or(0),
        observed_main,
        candidate_sha,
        candidate_ref,
        construction_author,
        construction_committer,
        construction_message,
        constructed_at: u64::try_from(constructed_at).unwrap_or(0),
        stage: decode_stage(&stage)?,
        reasons: decode_json("generation reasons", reasons)?,
        created_at: u64::try_from(created_at).unwrap_or(0),
        updated_at: u64::try_from(updated_at).unwrap_or(0),
    })
}

type ProofRaw = (
    String,
    String,
    String,
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
);

fn proof_from_row(row: &Row<'_>) -> rusqlite::Result<ProofRaw> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
    ))
}

fn decode_proof(raw: ProofRaw) -> Result<ProofRecord, VivariumError> {
    let (
        purpose,
        target,
        source_ref,
        generation,
        tip,
        rung,
        command,
        fingerprint,
        job,
        task,
        inputs,
        results,
        evidence,
        lines,
        created_at,
    ) = raw;
    Ok(ProofRecord {
        purpose: ProofPurpose::new(purpose),
        target,
        source_ref,
        generation: u64::try_from(generation).unwrap_or(0),
        tip,
        rung,
        command,
        fingerprint,
        job,
        task,
        inputs,
        results,
        evidence: decode_json("proof evidence", evidence)?,
        lines: decode_json("proof lines", lines)?,
        created_at: u64::try_from(created_at).unwrap_or(0),
    })
}

type IntentRaw = (
    i64,
    String,
    String,
    String,
    Option<i64>,
    Option<String>,
    Option<String>,
    String,
    i64,
    Option<i64>,
);

fn intent_from_row(row: &Row<'_>) -> rusqlite::Result<IntentRaw> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
}

fn decode_intent(raw: IntentRaw) -> Result<IntakeIntent, VivariumError> {
    let (
        id,
        kind,
        target,
        source_ref,
        generation,
        candidate_ref,
        candidate_sha,
        body,
        created_at,
        delivered_at,
    ) = raw;
    Ok(IntakeIntent {
        id,
        kind: IntakeIntentKind::parse(&kind)?,
        target,
        source_ref,
        generation: opt_u64(generation),
        candidate_ref,
        candidate_sha,
        body,
        created_at: u64::try_from(created_at).unwrap_or(0),
        delivered_at: opt_u64(delivered_at),
    })
}

fn opt_u64(value: Option<i64>) -> Option<u64> {
    value.and_then(|number| match u64::try_from(number) {
        Ok(number) => Some(number),
        Err(_) => None,
    })
}

fn opt_i64(value: Option<u64>) -> Option<i64> {
    value.and_then(|number| match i64::try_from(number) {
        Ok(number) => Some(number),
        Err(_) => None,
    })
}

impl PumpStore {
    /// Read one branch's complete intake state.
    ///
    /// # Errors
    /// Returns [`VivariumError`] for a partial schema, a malformed row, an
    /// invalid record, or an unknown stage word.
    pub fn branch_state(
        &self,
        target: &str,
        source_ref: &str,
    ) -> Result<Option<BranchState>, VivariumError> {
        let conn = self.conn();
        if !present(&conn)? {
            return Ok(None);
        }
        let row: Option<(Option<String>, Option<i64>, String, String, i64)> = conn
            .query_row(
                "SELECT latest_observation_json, active_generation,
                        notification_json, publication_json, updated_at
                   FROM branch_intake_state WHERE target = ?1 AND source_ref = ?2",
                params![target, source_ref],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(db_err("read branch intake state"))?;
        let Some((observation, active, notification, publication, updated_at)) = row else {
            return Ok(None);
        };
        let mut state = BranchState::new(target, source_ref);
        state.latest_observation = observation
            .map(|text| decode_json("latest observation", text))
            .transpose()?;
        state.active_generation = opt_u64(active);
        state.notification = decode_json("notification state", notification)?;
        state.publication = decode_json("publication state", publication)?;
        state.updated_at = u64::try_from(updated_at).unwrap_or(0);
        state.tips = read_tips(&conn, target, source_ref)?;
        state.generations = read_generations(&conn, target, source_ref)?;
        state.proofs = read_proofs(&conn, target, source_ref)?;
        state.intents = read_intents(&conn, target, source_ref)?;
        Ok(Some(state))
    }

    /// Read the current state of one branch. The same record as
    /// [`Self::branch_state`], named for callers that only need the head.
    ///
    /// # Errors
    /// As [`Self::branch_state`].
    pub fn current_branch_state(
        &self,
        target: &str,
        source_ref: &str,
    ) -> Result<Option<BranchState>, VivariumError> {
        self.branch_state(target, source_ref)
    }

    /// Write one branch's state. Tips, generations, proofs and intents are
    /// upserted, so a prior generation's provenance is never deleted.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when any row cannot be written.
    pub fn save_branch_state(&self, state: &BranchState) -> Result<(), VivariumError> {
        let observation = state
            .latest_observation
            .as_ref()
            .map(|value| encode_json("latest observation", value))
            .transpose()?;
        let notification = encode_json("notification state", &state.notification)?;
        let publication = encode_json("publication state", &state.publication)?;
        self.conn()
            .execute(
                "INSERT INTO branch_intake_state
                   (target, source_ref, latest_observation_json, active_generation,
                    notification_json, publication_json, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(target, source_ref) DO UPDATE SET
                   latest_observation_json = ?3, active_generation = ?4,
                   notification_json = ?5, publication_json = ?6, updated_at = ?7",
                params![
                    state.target,
                    state.source_ref,
                    observation,
                    opt_i64(state.active_generation),
                    notification,
                    publication,
                    i64::try_from(state.updated_at).unwrap_or(0),
                ],
            )
            .map_err(db_err("write branch intake state"))?;
        for tip in &state.tips {
            self.record_branch_tip(tip)?;
        }
        for generation in &state.generations {
            self.record_branch_generation(generation)?;
        }
        for proof in &state.proofs {
            self.record_branch_proof(proof)?;
        }
        for intent in &state.intents {
            self.record_branch_intent(intent)?;
        }
        Ok(())
    }

    /// Retain one observed tip, keyed by its full source SHA.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn record_branch_tip(&self, tip: &TipRecord) -> Result<(), VivariumError> {
        self.conn()
            .execute(
                "INSERT INTO branch_intake_tips
                   (target, source_ref, source_sha, observed_main, observed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(target, source_ref, source_sha) DO UPDATE SET
                   observed_main = ?4, observed_at = ?5",
                params![
                    tip.target,
                    tip.source_ref,
                    tip.source_sha,
                    tip.observed_main,
                    i64::try_from(tip.observed_at).unwrap_or(0),
                ],
            )
            .map_err(db_err("write branch intake tip"))?;
        Ok(())
    }

    /// Retain one generation. A later generation never removes an earlier one.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn record_branch_generation(
        &self,
        generation: &GenerationRecord,
    ) -> Result<(), VivariumError> {
        let reasons = encode_json("generation reasons", &generation.reasons)?;
        self.conn()
            .execute(
                "INSERT INTO branch_intake_generations
                   (target, source_ref, generation, observed_main, candidate_sha,
                    candidate_ref, construction_author, construction_committer,
                    construction_message, constructed_at, stage, reasons_json,
                    created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                 ON CONFLICT(target, source_ref, generation) DO UPDATE SET
                   observed_main = ?4, candidate_sha = ?5, candidate_ref = ?6,
                   construction_author = ?7, construction_committer = ?8,
                   construction_message = ?9, constructed_at = ?10, stage = ?11,
                   reasons_json = ?12, created_at = ?13, updated_at = ?14",
                params![
                    generation.target,
                    generation.source_ref,
                    i64::try_from(generation.generation).unwrap_or(0),
                    generation.observed_main,
                    generation.candidate_sha,
                    generation.candidate_ref,
                    generation.construction_author,
                    generation.construction_committer,
                    generation.construction_message,
                    i64::try_from(generation.constructed_at).unwrap_or(0),
                    generation.stage.as_str(),
                    reasons,
                    i64::try_from(generation.created_at).unwrap_or(0),
                    i64::try_from(generation.updated_at).unwrap_or(0),
                ],
            )
            .map_err(db_err("write branch intake generation"))?;
        Ok(())
    }

    /// Retain one proof record with its exact inputs, results and evidence.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn record_branch_proof(&self, proof: &ProofRecord) -> Result<(), VivariumError> {
        let evidence = encode_json("proof evidence", &proof.evidence)?;
        let lines = encode_json("proof lines", &proof.lines)?;
        self.conn()
            .execute(
                "INSERT INTO branch_intake_proofs
                   (purpose, target, source_ref, generation, tip, rung, command,
                    fingerprint, job, task, inputs, results, evidence_json,
                    lines_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    proof.purpose.as_str(),
                    proof.target,
                    proof.source_ref,
                    i64::try_from(proof.generation).unwrap_or(0),
                    proof.tip,
                    proof.rung,
                    proof.command,
                    proof.fingerprint,
                    proof.job,
                    proof.task,
                    proof.inputs,
                    proof.results,
                    evidence,
                    lines,
                    i64::try_from(proof.created_at).unwrap_or(0),
                ],
            )
            .map_err(db_err("write branch intake proof"))?;
        Ok(())
    }

    /// Retain one durable notification/publication intent. A nonzero `id`
    /// replaces that row; zero lets the database assign one.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn record_branch_intent(&self, intent: &IntakeIntent) -> Result<i64, VivariumError> {
        let conn = self.conn();
        let generation = opt_i64(intent.generation);
        let delivered = opt_i64(intent.delivered_at);
        if intent.id > 0 {
            conn.execute(
                "INSERT OR REPLACE INTO branch_intake_intents
                   (id, kind, target, source_ref, generation, candidate_ref,
                    candidate_sha, body, created_at, delivered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    intent.id,
                    intent.kind.as_str(),
                    intent.target,
                    intent.source_ref,
                    generation,
                    intent.candidate_ref,
                    intent.candidate_sha,
                    intent.body,
                    i64::try_from(intent.created_at).unwrap_or(0),
                    delivered,
                ],
            )
            .map_err(db_err("write branch intake intent"))?;
            return Ok(intent.id);
        }
        conn.execute(
            "INSERT INTO branch_intake_intents
               (kind, target, source_ref, generation, candidate_ref,
                candidate_sha, body, created_at, delivered_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                intent.kind.as_str(),
                intent.target,
                intent.source_ref,
                generation,
                intent.candidate_ref,
                intent.candidate_sha,
                intent.body,
                i64::try_from(intent.created_at).unwrap_or(0),
                delivered,
            ],
        )
        .map_err(db_err("write branch intake intent"))?;
        Ok(conn.last_insert_rowid())
    }

    /// Mark one durable intent delivered.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn mark_branch_intent_delivered(&self, id: i64, at: u64) -> Result<(), VivariumError> {
        let changed = self
            .conn()
            .execute(
                "UPDATE branch_intake_intents SET delivered_at = ?2 WHERE id = ?1",
                params![id, i64::try_from(at).unwrap_or(0)],
            )
            .map_err(db_err("mark branch intake intent delivered"))?;
        if changed == 0 {
            return Err(VivariumError::Other(format!(
                "pump.db branch intake: no intent {id}"
            )));
        }
        Ok(())
    }

    /// Read every retained generation, oldest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] for a corrupt or partial intake schema.
    pub fn branch_generations(
        &self,
        target: &str,
        source_ref: &str,
    ) -> Result<Vec<GenerationRecord>, VivariumError> {
        let conn = self.conn();
        if !present(&conn)? {
            return Ok(Vec::new());
        }
        read_generations(&conn, target, source_ref)
    }

    /// Read at most `limit` recent generations, newest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] for a corrupt or partial intake schema.
    pub fn recent_branch_generations(
        &self,
        target: &str,
        source_ref: &str,
        limit: usize,
    ) -> Result<Vec<GenerationRecord>, VivariumError> {
        let conn = self.conn();
        if !present(&conn)? {
            return Ok(Vec::new());
        }
        let mut stmt = conn
            .prepare(
                "SELECT target, source_ref, generation, observed_main, candidate_sha,
                        candidate_ref, construction_author, construction_committer,
                        construction_message, constructed_at, stage, reasons_json,
                        created_at, updated_at
                   FROM branch_intake_generations
                  WHERE target = ?1 AND source_ref = ?2
                  ORDER BY generation DESC LIMIT ?3",
            )
            .map_err(db_err("read recent branch intake generations"))?;
        let rows = stmt
            .query_map(
                params![target, source_ref, i64::try_from(limit).unwrap_or(i64::MAX)],
                generation_from_row,
            )
            .map_err(db_err("read recent branch intake generations"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read recent branch intake generations"))?
            .into_iter()
            .map(decode_generation)
            .collect()
    }

    /// Read the full tip history, oldest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] for a corrupt or partial intake schema.
    pub fn branch_tips(
        &self,
        target: &str,
        source_ref: &str,
    ) -> Result<Vec<TipRecord>, VivariumError> {
        let conn = self.conn();
        if !present(&conn)? {
            return Ok(Vec::new());
        }
        read_tips(&conn, target, source_ref)
    }

    /// Read one tip by its full source SHA.
    ///
    /// # Errors
    /// Returns [`VivariumError`] for a corrupt or partial intake schema.
    pub fn branch_tip(
        &self,
        target: &str,
        source_ref: &str,
        source_sha: &str,
    ) -> Result<Option<TipRecord>, VivariumError> {
        Ok(self
            .branch_tips(target, source_ref)?
            .into_iter()
            .find(|tip| tip.source_sha == source_sha))
    }

    /// Read every saved proof, oldest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] for a corrupt or partial intake schema.
    pub fn branch_proofs(
        &self,
        target: &str,
        source_ref: &str,
    ) -> Result<Vec<ProofRecord>, VivariumError> {
        let conn = self.conn();
        if !present(&conn)? {
            return Ok(Vec::new());
        }
        read_proofs(&conn, target, source_ref)
    }

    /// Read at most `limit` recent proofs, newest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] for a corrupt or partial intake schema.
    pub fn recent_branch_proofs(
        &self,
        target: &str,
        source_ref: &str,
        limit: usize,
    ) -> Result<Vec<ProofRecord>, VivariumError> {
        let mut proofs = self.branch_proofs(target, source_ref)?;
        proofs.reverse();
        proofs.truncate(limit);
        Ok(proofs)
    }

    /// Read every durable notification/publication intent, oldest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] for a corrupt or partial intake schema.
    pub fn branch_intents(
        &self,
        target: &str,
        source_ref: &str,
    ) -> Result<Vec<IntakeIntent>, VivariumError> {
        let conn = self.conn();
        if !present(&conn)? {
            return Ok(Vec::new());
        }
        read_intents(&conn, target, source_ref)
    }
}

fn read_tips(
    conn: &Connection,
    target: &str,
    source_ref: &str,
) -> Result<Vec<TipRecord>, VivariumError> {
    let mut stmt = conn
        .prepare(
            "SELECT source_sha, observed_main, observed_at FROM branch_intake_tips
              WHERE target = ?1 AND source_ref = ?2 ORDER BY observed_at, rowid",
        )
        .map_err(db_err("read branch intake tips"))?;
    let rows = stmt
        .query_map(params![target, source_ref], |row| {
            Ok(TipRecord {
                target: target.to_string(),
                source_ref: source_ref.to_string(),
                source_sha: row.get(0)?,
                observed_main: row.get(1)?,
                observed_at: u64::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
            })
        })
        .map_err(db_err("read branch intake tips"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read branch intake tips"))
}

fn read_generations(
    conn: &Connection,
    target: &str,
    source_ref: &str,
) -> Result<Vec<GenerationRecord>, VivariumError> {
    let mut stmt = conn
        .prepare(
            "SELECT target, source_ref, generation, observed_main, candidate_sha,
                    candidate_ref, construction_author, construction_committer,
                    construction_message, constructed_at, stage, reasons_json,
                    created_at, updated_at
               FROM branch_intake_generations
              WHERE target = ?1 AND source_ref = ?2 ORDER BY generation",
        )
        .map_err(db_err("read branch intake generations"))?;
    let rows = stmt
        .query_map(params![target, source_ref], generation_from_row)
        .map_err(db_err("read branch intake generations"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read branch intake generations"))?
        .into_iter()
        .map(decode_generation)
        .collect()
}

fn read_proofs(
    conn: &Connection,
    target: &str,
    source_ref: &str,
) -> Result<Vec<ProofRecord>, VivariumError> {
    let mut stmt = conn
        .prepare(
            "SELECT purpose, target, source_ref, generation, tip, rung, command,
                    fingerprint, job, task, inputs, results, evidence_json,
                    lines_json, created_at
               FROM branch_intake_proofs
              WHERE target = ?1 AND source_ref = ?2 ORDER BY id",
        )
        .map_err(db_err("read branch intake proofs"))?;
    let rows = stmt
        .query_map(params![target, source_ref], proof_from_row)
        .map_err(db_err("read branch intake proofs"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read branch intake proofs"))?
        .into_iter()
        .map(decode_proof)
        .collect()
}

fn read_intents(
    conn: &Connection,
    target: &str,
    source_ref: &str,
) -> Result<Vec<IntakeIntent>, VivariumError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, kind, target, source_ref, generation, candidate_ref,
                    candidate_sha, body, created_at, delivered_at
               FROM branch_intake_intents
              WHERE target = ?1 AND source_ref = ?2 ORDER BY id",
        )
        .map_err(db_err("read branch intake intents"))?;
    let rows = stmt
        .query_map(params![target, source_ref], intent_from_row)
        .map_err(db_err("read branch intake intents"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read branch intake intents"))?
        .into_iter()
        .map(decode_intent)
        .collect()
}

#[cfg(test)]
#[path = "branch_intake_test.rs"]
mod tests;
