//! Every SQL query the console runs, in one place.
//!
//! A page never carries SQL of its own: it names a [`Query`] from here. The
//! query plan test (`queries_test.rs`) walks [`REGISTRY`], runs
//! `EXPLAIN QUERY PLAN` for each entry against a seeded store, and fails when
//! a plan scans `slot_runs`, `mailspace_events` or `messages`. A page that
//! adds a query adds it to [`REGISTRY`] in the same change; a test also
//! refuses SQL text anywhere else in the crate's production sources.
//!
//! Every query is bounded by an indexed time window or a `LIMIT`. The index
//! names are those of the agreed `slot_runs` DDL (schema 11).

/// A parameter value to bind when the plan is explained.
#[derive(Debug, Clone, Copy)]
pub enum Sample {
    Int(i64),
    /// A time bound near the newest rows of the test store, so a range
    /// predicate is as selective as a real "last 24 hours" is.
    Recent,
    Text(&'static str),
}

/// One console query: a name for test output, the SQL, and sample parameters
/// for the plan check (one per `?N`, in order).
#[derive(Debug)]
pub struct Query {
    pub name: &'static str,
    pub sql: &'static str,
    pub samples: &'static [Sample],
}

/// Set on every console connection: any write is refused.
pub const QUERY_ONLY: &str = "PRAGMA query_only = true";

/// The columns of one `slot_runs` row as [`crate::data::runs::RunRow`] reads
/// them, in order.
macro_rules! run_columns {
    () => {
        "job, task_message_id, need, pool, slot, machine, model, kind, \
         dispatched_at, budget_s, collected_at, outcome, agent_rc, check_rc, \
         job_s, landing"
    };
}

/// A collected run counts as a failure when its outcome is not `finished`
/// or either exit code is non-zero. A missing code is not a failure.
macro_rules! failed_sql {
    () => {
        "(COALESCE(outcome, '') != 'finished' \
          OR COALESCE(agent_rc, 0) != 0 OR COALESCE(check_rc, 0) != 0)"
    };
}

/// Whether the store has a table of the given name (`?1`).
pub static HAS_TABLE: Query = Query {
    name: "has_table",
    sql: "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
    samples: &[Sample::Text("slot_runs")],
};

/// Runs that were dispatched and never collected: busy slots. `?1` caps the
/// rows. Sorted by the caller.
pub static OPEN_RUNS: Query = Query {
    name: "open_runs",
    sql: concat!(
        "SELECT ",
        run_columns!(),
        " FROM slot_runs WHERE collected_at IS NULL LIMIT ?1"
    ),
    samples: &[Sample::Int(1000)],
};

/// How many runs were dispatched since `?1` (epoch seconds).
pub static DISPATCHED_SINCE: Query = Query {
    name: "dispatched_since",
    sql: "SELECT COUNT(*) FROM slot_runs WHERE dispatched_at >= ?1",
    samples: &[Sample::Recent],
};

/// Runs collected since `?1`, counted by outcome and failure flag.
pub static OUTCOMES_SINCE: Query = Query {
    name: "outcomes_since",
    sql: concat!(
        "SELECT COALESCE(outcome, ''), CASE WHEN ",
        failed_sql!(),
        " THEN 1 ELSE 0 END, COUNT(*) FROM slot_runs \
         WHERE collected_at >= ?1 GROUP BY 1, 2"
    ),
    samples: &[Sample::Recent],
};

/// Runs collected since `?1`, counted by epoch hour and failure flag.
pub static HOURLY_SINCE: Query = Query {
    name: "hourly_since",
    sql: concat!(
        "SELECT collected_at / 3600, CASE WHEN ",
        failed_sql!(),
        " THEN 1 ELSE 0 END, COUNT(*) FROM slot_runs \
         WHERE collected_at >= ?1 GROUP BY 1, 2"
    ),
    samples: &[Sample::Recent],
};

/// The newest failed runs collected since `?1`, at most `?2`.
pub static FAILURES_SINCE: Query = Query {
    name: "failures_since",
    sql: concat!(
        "SELECT ",
        run_columns!(),
        " FROM slot_runs WHERE collected_at >= ?1 AND ",
        failed_sql!(),
        " ORDER BY collected_at DESC LIMIT ?2"
    ),
    samples: &[Sample::Recent, Sample::Int(10)],
};

/// The newest runs dispatched since `?1`, at most `?2`, newest first. The
/// caller keeps the first row it sees for each slot: that is the slot's
/// latest run. (A `GROUP BY pool, slot` would scan the slot index.)
pub static RECENT_RUNS: Query = Query {
    name: "recent_runs",
    sql: "SELECT pool, slot, machine, dispatched_at, outcome, collected_at \
          FROM slot_runs WHERE dispatched_at >= ?1 \
          ORDER BY dispatched_at DESC LIMIT ?2",
    samples: &[Sample::Recent, Sample::Int(5000)],
};

/// One slot's runs, newest first: pool `?1`, slot `?2`, at most `?3`.
pub static SLOT_HISTORY: Query = Query {
    name: "slot_history",
    sql: concat!(
        "SELECT ",
        run_columns!(),
        " FROM slot_runs WHERE pool = ?1 AND slot = ?2 \
          ORDER BY dispatched_at DESC LIMIT ?3"
    ),
    samples: &[Sample::Text("pool"), Sample::Text("slot"), Sample::Int(100)],
};

/// Every `slot_runs` column of one job, as [`crate::data::job::JobRun`] reads
/// them, in order.
macro_rules! job_columns {
    () => {
        "job, task_message_id, need, pool, slot, machine, model, kind, target, \
         branch, start_sha, budget_s, read_only, dispatched_at, collected_at, \
         outcome, push, sha, agent_rc, agent_s, check_rc, check_s, job_s, \
         agent_cpu_s, check_cpu_s, landing, landing_at, landing_reason"
    };
}

/// The jobs list page: runs dispatched in `[?1, ?2]`, newest first, at most
/// `?4`, resuming below the cursor `(?2, ?3)` (dispatch time, row id).
/// `?5..?8` are exact pool, slot and model and a need prefix; `?9` is an
/// outcome word or `open`; `?10` is 1 to keep only failures. An unused filter
/// is NULL (0 for `?10`). Only the time range uses an index (the dispatch
/// time index, in order, so no sort); the other filters are applied to the
/// rows of that range, which a window of at most seven days bounds.
pub static JOBS_LIST: Query = Query {
    name: "jobs_list",
    sql: concat!(
        "SELECT ",
        run_columns!(),
        ", rowid FROM slot_runs \
         WHERE dispatched_at >= ?1 AND dispatched_at <= ?2 \
           AND (dispatched_at < ?2 OR rowid < ?3) \
           AND (?5 IS NULL OR pool = ?5) \
           AND (?6 IS NULL OR slot = ?6) \
           AND (?7 IS NULL OR model = ?7) \
           AND (?8 IS NULL OR substr(need, 1, length(?8)) = ?8) \
           AND (?9 IS NULL OR (?9 = 'open' AND collected_at IS NULL) \
                OR (?9 != 'open' AND outcome = ?9)) \
           AND (?10 = 0 OR (collected_at IS NOT NULL AND ",
        failed_sql!(),
        ") OR landing IN ('red', 'conflict', 'raced')) \
         ORDER BY dispatched_at DESC, rowid DESC LIMIT ?4"
    ),
    samples: &[
        Sample::Recent,
        Sample::Int(i64::MAX),
        Sample::Int(i64::MAX),
        Sample::Int(101),
        Sample::Text("pool"),
        Sample::Text("slot"),
        Sample::Text("model"),
        Sample::Text("need"),
        Sample::Text("finished"),
        Sample::Int(1),
    ],
};

/// One job's full row, by its primary key `?1`.
pub static JOB_RUN: Query = Query {
    name: "job_run",
    sql: concat!("SELECT ", job_columns!(), " FROM slot_runs WHERE job = ?1"),
    samples: &[Sample::Text("job-1")],
};

/// A task's events in time order, at most `?2`: message `?1`.
pub static TASK_EVENTS: Query = Query {
    name: "task_events",
    sql: "SELECT occurred_at, event_type, command, actor_identity, note \
          FROM mailspace_events WHERE message_id = ?1 \
          ORDER BY occurred_at, event_id LIMIT ?2",
    samples: &[Sample::Text("msg_0123456789abcdef0123"), Sample::Int(500)],
};

/// Where a message's text lives: blob path, content id, date, sender and
/// subject of message `?1`.
pub static MESSAGE_BLOB: Query = Query {
    name: "message_blob",
    sql: "SELECT b.blob_relpath, m.content_id, md.date, md.from_addr, md.subject \
          FROM messages m JOIN blobs b ON b.content_id = m.content_id \
          LEFT JOIN message_metadata md ON md.content_id = m.content_id \
          WHERE m.message_id = ?1",
    samples: &[Sample::Text("msg_0123456789abcdef0123")],
};

/// The letters linked as replies to the letter with content `?1`, at most
/// `?2`: content id, blob path, date, sender and subject.
pub static THREAD_CHILDREN: Query = Query {
    name: "thread_children",
    sql: "SELECT l.child_content_id, b.blob_relpath, md.date, md.from_addr, md.subject \
          FROM mailspace_links l JOIN blobs b ON b.content_id = l.child_content_id \
          LEFT JOIN message_metadata md ON md.content_id = l.child_content_id \
          WHERE l.parent_content_id = ?1 ORDER BY l.rowid LIMIT ?2",
    samples: &[Sample::Text("content"), Sample::Int(200)],
};

/// Every query the console runs. Later pages add theirs here.
pub static REGISTRY: &[&Query] = &[
    &HAS_TABLE,
    &OPEN_RUNS,
    &DISPATCHED_SINCE,
    &OUTCOMES_SINCE,
    &HOURLY_SINCE,
    &FAILURES_SINCE,
    &RECENT_RUNS,
    &SLOT_HISTORY,
    &JOBS_LIST,
    &JOB_RUN,
    &TASK_EVENTS,
    &MESSAGE_BLOB,
    &THREAD_CHILDREN,
    &FAILURE_KINDS,
    &LANDING_REASONS,
    &NEED_KINDS,
    &MAIL_THREADS,
    &MAIL_RESOLVE,
    &MAIL_THREAD,
];

// ---- Failures page (mailspace store) -------------------------------------

/// The failure kind of a collected `slot_runs` row, as a SQL expression.
/// The first matching line wins; `landed` and `ok` are not failures. A
/// finished, clean run counts as `not_pushed` when it neither pushed nor
/// committed and was not a read-only job.
macro_rules! kind_sql {
    () => {
        "CASE \
           WHEN outcome = 'timeout' THEN 'timeout' \
           WHEN outcome = 'lost' THEN 'lost' \
           WHEN COALESCE(outcome, '') != 'finished' THEN 'other' \
           WHEN COALESCE(agent_rc, 0) != 0 THEN 'agent' \
           WHEN COALESCE(check_rc, 0) != 0 THEN 'check' \
           WHEN landing = 'red' THEN 'landing_red' \
           WHEN landing = 'conflict' THEN 'conflict' \
           WHEN landing = 'raced' THEN 'raced' \
           WHEN landing = 'landed' THEN 'landed' \
           WHEN read_only = 0 AND COALESCE(push, '') NOT IN ('pushed', 'committed') \
             THEN 'not_pushed' \
           ELSE 'ok' END"
    };
}

/// Runs collected since `?1`, counted by failure kind and by period: `0` is
/// at or after `?2` (the window), `1` is before it (the window before).
/// `?3` is a pool name, or `''` for every pool.
pub static FAILURE_KINDS: Query = Query {
    name: "failure_kinds",
    sql: concat!(
        "SELECT ",
        kind_sql!(),
        ", CASE WHEN collected_at >= ?2 THEN 0 ELSE 1 END, COUNT(*) FROM slot_runs \
         WHERE collected_at >= ?1 AND (?3 = '' OR pool = ?3) GROUP BY 1, 2"
    ),
    samples: &[Sample::Recent, Sample::Recent, Sample::Text("")],
};

/// Landings that conflicted or went red since `?1`, with their recorded
/// reason, counted. `?2` is a pool name or `''`; `?3` caps the groups.
pub static LANDING_REASONS: Query = Query {
    name: "landing_reasons",
    sql: "SELECT landing, landing_reason, COUNT(*) FROM slot_runs \
          WHERE collected_at >= ?1 AND landing IN ('conflict', 'red') \
          AND landing_reason IS NOT NULL AND (?2 = '' OR pool = ?2) \
          GROUP BY 1, 2 LIMIT ?3",
    samples: &[Sample::Recent, Sample::Text(""), Sample::Int(5000)],
};

/// Runs of needs collected since `?1`, counted by need and failure kind.
/// `?2` is a pool name or `''`; `?3` caps the groups. The time index is
/// named because, left alone, the planner walks the whole need index to
/// avoid sorting the groups.
pub static NEED_KINDS: Query = Query {
    name: "need_kinds",
    sql: concat!(
        "SELECT need, ",
        kind_sql!(),
        ", COUNT(*) FROM slot_runs INDEXED BY slot_runs_collected_idx \
         WHERE collected_at >= ?1 AND COALESCE(need, '') != '' AND (?2 = '' OR pool = ?2) \
         GROUP BY 1, 2 LIMIT ?3"
    ),
    samples: &[Sample::Recent, Sample::Text(""), Sample::Int(20000)],
};

// ---- Mail page (mailspace store) ------------------------------------------
//
// No alias on `messages`: the plan check looks for `SCAN messages`.

/// Thread roots filed under account `?1` and folder `?2`, newest first, at
/// most `?3`. A root is a letter no `mailspace_links` row names as a child.
/// The last column counts its direct replies.
pub static MAIL_THREADS: Query = Query {
    name: "mail_threads",
    sql: "SELECT messages.message_id, messages.account, messages.local_role, \
          messages.content_id, messages.updated_at, message_metadata.date, \
          message_metadata.from_addr, message_metadata.subject, \
          (SELECT COUNT(*) FROM mailspace_links \
             WHERE mailspace_links.parent_content_id = messages.content_id) \
          FROM messages JOIN message_metadata \
            ON message_metadata.content_id = messages.content_id \
          WHERE messages.account = ?1 AND messages.local_role = ?2 \
            AND messages.deleted_at IS NULL \
            AND NOT EXISTS (SELECT 1 FROM mailspace_links \
                              WHERE mailspace_links.child_content_id = messages.content_id) \
          ORDER BY messages.updated_at DESC LIMIT ?3",
    samples: &[Sample::Text("a"), Sample::Text("inbox"), Sample::Int(50)],
};

/// Messages whose id is in the range `[?1, ?2)`: how a handle (a prefix of
/// an id) resolves. At most three rows, so ambiguity shows.
pub static MAIL_RESOLVE: Query = Query {
    name: "mail_resolve",
    sql: "SELECT messages.message_id, messages.content_id FROM messages \
          WHERE messages.message_id >= ?1 AND messages.message_id < ?2 \
            AND messages.deleted_at IS NULL LIMIT 3",
    samples: &[Sample::Text("msg_a"), Sample::Text("msg_b")],
};

/// Every letter of the thread holding content `?1`, oldest first, at most
/// `?2`: the ancestors of `?1`, then everything below them. One row per
/// content: its metadata and blob path, the id of one live message holding
/// it, when it was first stored, and its parent content (if linked).
pub static MAIL_THREAD: Query = Query {
    name: "mail_thread",
    sql: "WITH RECURSIVE \
          up(cid) AS ( \
            SELECT ?1 \
            UNION SELECT parent_content_id FROM mailspace_links, up \
              WHERE child_content_id = up.cid), \
          comp(cid) AS ( \
            SELECT cid FROM up \
            UNION SELECT child_content_id FROM mailspace_links, comp \
              WHERE parent_content_id = comp.cid) \
          SELECT message_metadata.content_id, message_metadata.date, \
            message_metadata.from_addr, message_metadata.to_addr, \
            message_metadata.cc_addr, message_metadata.subject, blobs.blob_relpath, \
            (SELECT messages.message_id FROM messages \
               WHERE messages.content_id = message_metadata.content_id \
                 AND messages.deleted_at IS NULL \
               ORDER BY messages.message_id LIMIT 1), \
            (SELECT MIN(messages.discovered_at) FROM messages \
               WHERE messages.content_id = message_metadata.content_id), \
            (SELECT mailspace_links.parent_content_id FROM mailspace_links \
               WHERE mailspace_links.child_content_id = message_metadata.content_id) \
          FROM comp CROSS JOIN message_metadata \
            ON message_metadata.content_id = comp.cid \
          CROSS JOIN blobs ON blobs.content_id = message_metadata.content_id \
          ORDER BY message_metadata.date, message_metadata.content_id LIMIT ?2",
    samples: &[Sample::Text("c"), Sample::Int(200)],
};

// ---- Pump page (pump.db) ----------------------------------------------------
//
// These run on the pump store, not the mailspace store, so they have their
// own list and plan check ([`PUMP_REGISTRY`]). Newest-first reads use a
// rowid range (`> ?1` with `0`) so the plan is a SEARCH walked from the end,
// never a SCAN.

/// The pump's control mode.
pub static PUMP_CONTROL: Query = Query {
    name: "pump_control",
    sql: "SELECT mode, shutdown_requested FROM pump_control WHERE singleton = 1",
    samples: &[],
};

/// Units counted by status. `?1` is `''`: every status sorts above it, and
/// the range makes the grouped count a covering-index search.
pub static PUMP_UNIT_COUNTS: Query = Query {
    name: "pump_unit_counts",
    sql: "SELECT status, COUNT(*) FROM units WHERE status > ?1 GROUP BY status",
    samples: &[Sample::Text("")],
};

/// The newest unit events: ids above `?1`, newest first, at most `?2`.
pub static PUMP_UNIT_EVENTS: Query = Query {
    name: "pump_unit_events",
    sql: "SELECT event_id, unit, attempt, model, why, detail, at FROM unit_events \
          WHERE event_id > ?1 ORDER BY event_id DESC LIMIT ?2",
    samples: &[Sample::Int(0), Sample::Int(50)],
};

/// The newest gates: ids above `?1`, newest first, at most `?2`.
pub static PUMP_GATES: Query = Query {
    name: "pump_gates",
    sql: "SELECT gate_id, tip, at, merges FROM gates \
          WHERE gate_id > ?1 ORDER BY gate_id DESC LIMIT ?2",
    samples: &[Sample::Int(0), Sample::Int(10)],
};

/// Failures of the gates with ids from `?1` to `?2`, at most `?3`.
pub static PUMP_GATE_FAILURES: Query = Query {
    name: "pump_gate_failures",
    sql: "SELECT gate_id, name, kind FROM gate_failures \
          WHERE gate_id >= ?1 AND gate_id <= ?2 LIMIT ?3",
    samples: &[Sample::Int(190), Sample::Int(200), Sample::Int(500)],
};

/// The newest exceptions: ids above `?1`, newest first, at most `?2`.
pub static PUMP_EXCEPTIONS: Query = Query {
    name: "pump_exceptions",
    sql: "SELECT event_id, at, kind, unit, text FROM exceptions \
          WHERE event_id > ?1 ORDER BY event_id DESC LIMIT ?2",
    samples: &[Sample::Int(0), Sample::Int(20)],
};

/// The tasks of the newest flow steps: rowids above `?1`, newest first, at
/// most `?2`. Rung runs have no time index, so the page reads them by task.
pub static PUMP_RECENT_STEP_TASKS: Query = Query {
    name: "pump_recent_step_tasks",
    sql: "SELECT task FROM flow_steps WHERE rowid > ?1 ORDER BY rowid DESC LIMIT ?2",
    samples: &[Sample::Int(0), Sample::Int(40)],
};

/// The rung runs of one task (`?1`).
pub static PUMP_RUNG_RUNS: Query = Query {
    name: "pump_rung_runs",
    sql: "SELECT task, rung, started_at, finished_at, rc FROM rung_runs WHERE task = ?1",
    samples: &[Sample::Text("task")],
};

/// Every query that runs on `pump.db`. Checked by `pump_plans_never_scan`.
pub static PUMP_REGISTRY: &[&Query] = &[
    &PUMP_CONTROL,
    &PUMP_UNIT_COUNTS,
    &PUMP_UNIT_EVENTS,
    &PUMP_GATES,
    &PUMP_GATE_FAILURES,
    &PUMP_EXCEPTIONS,
    &PUMP_RECENT_STEP_TASKS,
    &PUMP_RUNG_RUNS,
];

#[cfg(test)]
#[path = "queries_test.rs"]
mod tests;
