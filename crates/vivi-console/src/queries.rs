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
];

#[cfg(test)]
#[path = "queries_test.rs"]
mod tests;
