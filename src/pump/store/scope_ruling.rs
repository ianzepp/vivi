//! Immutable scope rulings of `pump.db`: one row per blocked-scope request.
//!
//! A ruling records who asked (the originating request job identity), what
//! was asked (usable requested paths stored normalized, unusable
//! declarations kept as evidence), and how the request was settled (the
//! typed approved/operator decision, the typed policy reason for a policy
//! denial, and the target scope policy snapshot the decision was made
//! under). The row is the durable grant authority; thread text never is.
//! The target's whole allow list is deliberately not copied into the grant —
//! only the requested paths are.
//!
//! Like the flow tables, the table is created with `IF NOT EXISTS` on every
//! open and is not part of the [`State`](crate::pump::state::State) save, so
//! `save()` and `clear()` never touch it and the schema version is not
//! bumped here. A replay of the same request identity returns the original
//! decision; a different request under the same identity is refused, so
//! historical authorization is never overwritten.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, params};
use sha2::{Digest, Sha256};

use crate::error::VivariumError;
use crate::pump::config::scope::{ScopePath, ScopePolicy, ScopeRejection};
use crate::pump::envelope::correct;

use super::{PumpStore, db_err, from_u32, json};

/// One requested path declaration that could not be normalized, kept as
/// request evidence. Evidence never counts as a grant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InvalidScopePath {
    /// The declaration exactly as requested.
    pub path: String,
    /// Why the declaration is unusable.
    pub problem: String,
}

/// The settled decision of a scope ruling: the durable grant authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulingDecision {
    /// Approved under the target's scope policy.
    Approved,
    /// Denied under the target's scope policy; the ruling carries the typed
    /// policy reason.
    Denied,
    /// An operator settled the request: granted.
    OperatorApproved,
    /// An operator settled the request: denied.
    OperatorDenied,
}

impl RulingDecision {
    /// The word stored in `scope_rulings.decision`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::OperatorApproved => "operator-approved",
            Self::OperatorDenied => "operator-denied",
        }
    }

    /// Read a stored decision word.
    ///
    /// # Errors
    /// Returns [`VivariumError`] for a word that is not a ruling decision.
    pub fn parse(word: &str) -> Result<Self, VivariumError> {
        Ok(match word {
            "approved" => Self::Approved,
            "denied" => Self::Denied,
            "operator-approved" => Self::OperatorApproved,
            "operator-denied" => Self::OperatorDenied,
            other => {
                return Err(VivariumError::Other(format!(
                    "pump.db scope_rulings: unknown ruling decision {other:?}"
                )));
            }
        })
    }

    /// True when the decision grants the requested paths.
    #[must_use]
    pub fn is_grant(self) -> bool {
        matches!(self, Self::Approved | Self::OperatorApproved)
    }
}

/// A scope ruling as the caller presents it: the originating request job
/// identity, what was asked, and how the request was settled. Requested
/// paths arrive as raw declarations; the store normalizes the usable ones
/// and keeps the rest as evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRulingRequest {
    /// The originating request job id: the ruling's origin key.
    pub job: String,
    /// The kind of the originating request job.
    pub job_kind: String,
    /// The need the request belongs to.
    pub need: String,
    /// The run the request belongs to.
    pub run: u32,
    /// The flow node the request came from.
    pub node: String,
    /// The task handle that made the request.
    pub task: String,
    /// The role that made the request.
    pub role: String,
    /// The target the requested paths are relative to.
    pub target: String,
    /// The requested path declarations, as requested.
    pub paths: Vec<String>,
    /// The reason the request stated.
    pub reason: String,
    /// How the request was settled.
    pub decision: RulingDecision,
    /// The typed policy reason, for a policy denial.
    pub policy_reason: Option<ScopeRejection>,
    /// The resolved target scope policy the decision was made under.
    pub policy: ScopePolicy,
}

/// One stored, immutable scope ruling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRulingRow {
    /// The originating request job id: the ruling's origin key.
    pub job: String,
    /// The kind of the originating request job.
    pub job_kind: String,
    /// The need the request belongs to.
    pub need: String,
    /// The run the request belongs to.
    pub run: u32,
    /// The flow node the request came from.
    pub node: String,
    /// The task handle that made the request.
    pub task: String,
    /// The role that made the request.
    pub role: String,
    /// The target the requested paths are relative to.
    pub target: String,
    /// The requested paths that normalized, in canonical form.
    pub paths: Vec<String>,
    /// Requested declarations that could not be normalized: evidence only.
    pub invalid: Vec<InvalidScopePath>,
    /// The reason the request stated.
    pub reason: String,
    /// How the request was settled.
    pub decision: RulingDecision,
    /// The typed policy reason, for a policy denial.
    pub policy_reason: Option<ScopeRejection>,
    /// The resolved target scope policy the decision was made under.
    pub policy: ScopePolicy,
    /// The deterministic decision reference; `None` until decided.
    pub decision_ref: Option<String>,
    /// The reply body of the decision letter; `None` until written.
    pub reply_body: Option<String>,
    /// When the decision letter was posted to the thread (unix seconds);
    /// `None` until posted.
    pub posted_at: Option<i64>,
    /// The deterministic continuation identity; `None` until set.
    pub continuation_id: Option<String>,
    /// When the continuation was posted (unix seconds); `None` until posted.
    pub continuation_posted_at: Option<i64>,
    /// When the ruling was stored (unix seconds).
    pub created_at: i64,
}

/// The deterministic decision reference of a ruling, derived from the origin
/// job identity and the decision: the same ruling always carries the same
/// reference, so a redrafted decision letter stays recognizable.
#[must_use]
pub fn decision_reference(job: &str, decision: RulingDecision) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"scope-ruling\n");
    hasher.update(job.as_bytes());
    hasher.update(b"\n");
    hasher.update(decision.word().as_bytes());
    format!("scope-{}", hex::encode(&hasher.finalize()[..8]))
}

/// The deterministic continuation identity of the ruling for `job`: the next
/// correction job in `job`'s chain, matching the envelope correction ids.
#[must_use]
pub fn continuation_id(job: &str) -> String {
    format!(
        "{}-c{}",
        correct::base_id(job),
        correct::correction_n(job) + 1
    )
}

const DDL: &str = "
CREATE TABLE IF NOT EXISTS scope_rulings (
    job TEXT PRIMARY KEY,
    job_kind TEXT NOT NULL,
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    node TEXT NOT NULL,
    task TEXT NOT NULL,
    role TEXT NOT NULL,
    target TEXT NOT NULL,
    paths_json TEXT NOT NULL,
    invalid_json TEXT NOT NULL,
    reason TEXT NOT NULL,
    decision TEXT NOT NULL,
    policy_reason_json TEXT,
    policy_json TEXT NOT NULL,
    decision_ref TEXT,
    reply_body TEXT,
    posted_at INTEGER,
    continuation_id TEXT,
    continuation_posted_at INTEGER,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS scope_rulings_task ON scope_rulings (task);
";

const COLUMNS: &str = "job, job_kind, need, run, node, task, role, target, paths_json, \
                       invalid_json, reason, decision, policy_reason_json, policy_json, \
                       decision_ref, reply_body, posted_at, continuation_id, \
                       continuation_posted_at, created_at";

/// Create the scope ruling table when it is missing.
pub(super) fn ensure_table(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create scope ruling table"))
}

impl PumpStore {
    /// Store a scope ruling keyed by its originating request job identity.
    ///
    /// The first insert for a job stores the ruling: usable requested paths
    /// normalized, unusable declarations kept as evidence. A replay of the
    /// same identity returns the original decision unchanged; a different
    /// request under the same identity is refused, so historical
    /// authorization is never overwritten.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when an existing ruling has an incompatible
    /// identity, or the row cannot be written or read back.
    pub fn insert_scope_ruling(
        &self,
        request: &ScopeRulingRequest,
    ) -> Result<ScopeRulingRow, VivariumError> {
        self.insert_or_reuse(stored_row(request))
    }

    fn insert_or_reuse(&self, row: ScopeRulingRow) -> Result<ScopeRulingRow, VivariumError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin scope ruling"))?;
        let stored = match read_by_job(&tx, &row.job)? {
            Some(existing) => {
                if let Some(why) = identity_conflict(&existing, &row) {
                    return Err(VivariumError::Other(why));
                }
                existing
            }
            None => {
                write_ruling(&tx, &row)?;
                row
            }
        };
        tx.commit().map_err(db_err("commit scope ruling"))?;
        Ok(stored)
    }

    /// Read one ruling by its originating request job id.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read or decoded.
    pub fn scope_ruling(&self, job: &str) -> Result<Option<ScopeRulingRow>, VivariumError> {
        Ok(self
            .rulings(
                &format!("SELECT {COLUMNS} FROM scope_rulings WHERE job = ?1"),
                [job],
            )?
            .pop())
    }

    /// Every ruling `task`'s request job produced, oldest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the rows cannot be read or decoded.
    pub fn scope_rulings_for_task(&self, task: &str) -> Result<Vec<ScopeRulingRow>, VivariumError> {
        self.rulings(
            &format!(
                "SELECT {COLUMNS} FROM scope_rulings WHERE task = ?1 ORDER BY created_at, job"
            ),
            [task],
        )
    }

    fn rulings(
        &self,
        sql: &str,
        args: impl rusqlite::Params,
    ) -> Result<Vec<ScopeRulingRow>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(db_err("read scope rulings"))?;
        let rows = stmt
            .query_map(args, raw_ruling)
            .map_err(db_err("read scope rulings"))?;
        rows.map(|raw| match raw {
            Ok(tuple) => finish_ruling(tuple),
            Err(error) => Err(db_err("read scope rulings")(error)),
        })
        .collect()
    }
}

/// The stored form of a request: paths normalized, evidence kept, the
/// decision reference derived, the progress fields still untouched.
fn stored_row(request: &ScopeRulingRequest) -> ScopeRulingRow {
    let (paths, invalid) = normalize_paths(&request.paths);
    ScopeRulingRow {
        job: request.job.clone(),
        job_kind: request.job_kind.clone(),
        need: request.need.clone(),
        run: request.run,
        node: request.node.clone(),
        task: request.task.clone(),
        role: request.role.clone(),
        target: request.target.clone(),
        paths,
        invalid,
        reason: request.reason.clone(),
        decision: request.decision,
        policy_reason: request.policy_reason.clone(),
        policy: request.policy.clone(),
        decision_ref: Some(decision_reference(&request.job, request.decision)),
        reply_body: None,
        posted_at: None,
        continuation_id: None,
        continuation_posted_at: None,
        created_at: now_unix(),
    }
}

/// Split requested declarations into normalized canonical paths and
/// unusable-declaration evidence, preserving request order.
fn normalize_paths(paths: &[String]) -> (Vec<String>, Vec<InvalidScopePath>) {
    let mut valid = Vec::new();
    let mut invalid = Vec::new();
    for raw in paths {
        match ScopePath::parse(raw) {
            Ok(path) => {
                let canonical = path.canonical();
                if !valid.contains(&canonical) {
                    valid.push(canonical);
                }
            }
            Err(error) => invalid.push(InvalidScopePath {
                path: raw.clone(),
                problem: error.to_string(),
            }),
        }
    }
    (valid, invalid)
}

/// The first request identity field where an incoming request disagrees with
/// the stored ruling. The decision is not identity: a replay may settle
/// differently and still reuse the original ruling.
fn identity_conflict(old: &ScopeRulingRow, incoming: &ScopeRulingRow) -> Option<String> {
    let fields = [
        ("job_kind", old.job_kind != incoming.job_kind),
        ("need", old.need != incoming.need),
        ("run", old.run != incoming.run),
        ("node", old.node != incoming.node),
        ("task", old.task != incoming.task),
        ("role", old.role != incoming.role),
        ("target", old.target != incoming.target),
        ("paths", old.paths != incoming.paths),
        ("invalid", old.invalid != incoming.invalid),
        ("reason", old.reason != incoming.reason),
    ];
    let (field, _) = fields.into_iter().find(|(_, differs)| *differs)?;
    Some(format!(
        "pump.db scope_rulings: job {} already holds a different scope ruling \
         ({field} differs); the stored ruling is immutable and was not changed",
        incoming.job
    ))
}

/// A ruling with its plain columns decoded and its five encoded fields still
/// raw: paths, invalid evidence, decision word, policy reason, policy.
type RawRuling = (
    ScopeRulingRow,
    String,
    String,
    String,
    Option<String>,
    String,
);

fn raw_ruling(row: &Row<'_>) -> rusqlite::Result<RawRuling> {
    Ok((
        ScopeRulingRow {
            job: row.get(0)?,
            job_kind: row.get(1)?,
            need: row.get(2)?,
            run: from_u32(row.get(3)?),
            node: row.get(4)?,
            task: row.get(5)?,
            role: row.get(6)?,
            target: row.get(7)?,
            paths: Vec::new(),
            invalid: Vec::new(),
            reason: row.get(10)?,
            decision: RulingDecision::Approved,
            policy_reason: None,
            policy: ScopePolicy {
                target: String::new(),
                allow: Vec::new(),
                deny: Vec::new(),
            },
            decision_ref: row.get(14)?,
            reply_body: row.get(15)?,
            posted_at: row.get(16)?,
            continuation_id: row.get(17)?,
            continuation_posted_at: row.get(18)?,
            created_at: row.get(19)?,
        },
        row.get(8)?,
        row.get(9)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
    ))
}

fn finish_ruling(tuple: RawRuling) -> Result<ScopeRulingRow, VivariumError> {
    let (mut ruling, paths, invalid, decision, policy_reason, policy) = tuple;
    ruling.paths = serde_json::from_str(&paths).map_err(db_err("decode scope ruling paths"))?;
    ruling.invalid =
        serde_json::from_str(&invalid).map_err(db_err("decode scope ruling evidence"))?;
    ruling.decision = RulingDecision::parse(&decision)?;
    ruling.policy_reason = policy_reason
        .map(|text| {
            serde_json::from_str(&text).map_err(db_err("decode scope ruling policy reason"))
        })
        .transpose()?;
    ruling.policy = serde_json::from_str(&policy).map_err(db_err("decode scope ruling policy"))?;
    Ok(ruling)
}

fn read_by_job(conn: &Connection, job: &str) -> Result<Option<ScopeRulingRow>, VivariumError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM scope_rulings WHERE job = ?1"),
        [job],
        raw_ruling,
    )
    .optional()
    .map_err(db_err("read scope ruling"))?
    .map(finish_ruling)
    .transpose()
}

fn write_ruling(conn: &Connection, row: &ScopeRulingRow) -> Result<(), VivariumError> {
    let policy_reason = row.policy_reason.as_ref().map(json).transpose()?;
    conn.execute(
        "INSERT INTO scope_rulings (
             job, job_kind, need, run, node, task, role, target, paths_json,
             invalid_json, reason, decision, policy_reason_json, policy_json,
             decision_ref, reply_body, posted_at, continuation_id,
             continuation_posted_at, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                   ?15, ?16, ?17, ?18, ?19, ?20)",
        params![
            row.job,
            row.job_kind,
            row.need,
            i64::from(row.run),
            row.node,
            row.task,
            row.role,
            row.target,
            json(&row.paths)?,
            json(&row.invalid)?,
            row.reason,
            row.decision.word(),
            policy_reason,
            json(&row.policy)?,
            row.decision_ref,
            row.reply_body,
            row.posted_at,
            row.continuation_id,
            row.continuation_posted_at,
            row.created_at,
        ],
    )
    .map_err(db_err("insert scope ruling"))?;
    Ok(())
}

fn now_unix() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or(0)
}

#[cfg(test)]
#[path = "scope_ruling_test.rs"]
mod tests;
