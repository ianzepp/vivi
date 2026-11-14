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

// ---- Needs and the need page: mailspace store ----

/// The columns of one backlog node as [`crate::data::needs::NeedNode`] reads
/// them, in order: handle, subject, state, created (RFC 3339).
macro_rules! need_columns {
    () => {
        "n.source_id, n.label, n.state, n.created_at"
    };
}

/// The backlog graph's need nodes in the states `?1..?3`, newest first, at
/// most `?4`. The backlog graph is the one whose code is `backlog`; every need
/// filed with a work graph attached has a node, keyed by the need's handle.
pub static NEEDS_LIST: Query = Query {
    name: "needs_list",
    sql: concat!(
        "SELECT ",
        need_columns!(),
        " FROM work_graph_nodes n \
         WHERE n.graph_handle = (SELECT handle FROM work_graphs WHERE code = 'backlog') \
         AND n.kind = 'need' AND n.state IN (?1, ?2, ?3) \
         ORDER BY n.created_at DESC LIMIT ?4"
    ),
    samples: &[
        Sample::Text("open"),
        Sample::Text("active"),
        Sample::Text("done"),
        Sample::Int(300),
    ],
};

/// One need node by its handle (`?1`).
pub static NEED_NODE: Query = Query {
    name: "need_node",
    sql: concat!(
        "SELECT ",
        need_columns!(),
        " FROM work_graph_nodes n \
         WHERE n.graph_handle = (SELECT handle FROM work_graphs WHERE code = 'backlog') \
         AND n.source_id = ?1 AND n.kind = 'need'"
    ),
    samples: &[Sample::Text("a1f3c2d0")],
};

/// The active message ids from `?1` (inclusive) to `?2` (exclusive): the
/// copies of a need whose handle is a prefix of their id. A primary-key range.
pub static NEED_MESSAGE: Query = Query {
    name: "need_message",
    sql: "SELECT message_id, content_id, local_role FROM messages \
          WHERE message_id >= ?1 AND message_id < ?2 AND deleted_at IS NULL \
          ORDER BY message_id LIMIT 4",
    samples: &[Sample::Text("msg_a1f3c2d0"), Sample::Text("msg_a1f3c2d1")],
};

/// One letter by content id (`?1`): its date, sender, subject and where its
/// raw bytes live under the mailspace directory.
pub static LETTER_OF_CONTENT: Query = Query {
    name: "letter_of_content",
    sql: "SELECT md.date, md.from_addr, md.subject, b.blob_relpath \
          FROM message_metadata md JOIN blobs b ON b.content_id = md.content_id \
          WHERE md.content_id = ?1",
    samples: &[Sample::Text("content")],
};

/// The letters linked below the content `?1` (replies, and replies to
/// replies down to eight levels), oldest level first, at most `?2`. The
/// `CROSS JOIN`s fix the join order: the planner would otherwise scan the
/// metadata table and index the recursive result.
pub static THREAD_OF_CONTENT: Query = Query {
    name: "thread_of_content",
    sql: "WITH RECURSIVE thread(cid, depth) AS ( \
            SELECT ?1, 0 \
            UNION \
            SELECT l.child_content_id, thread.depth + 1 \
            FROM mailspace_links l JOIN thread ON l.parent_content_id = thread.cid \
            WHERE thread.depth < 8) \
          SELECT t.cid, md.date, md.from_addr, md.subject, b.blob_relpath \
          FROM thread t \
          CROSS JOIN message_metadata md ON md.content_id = t.cid \
          CROSS JOIN blobs b ON b.content_id = t.cid \
          WHERE t.depth > 0 ORDER BY t.depth, md.date LIMIT ?2",
    samples: &[Sample::Text("content"), Sample::Int(60)],
};

/// How many runs the slot index holds for the need `?1`.
pub static NEED_ATTEMPT_COUNT: Query = Query {
    name: "need_attempt_count",
    sql: "SELECT COUNT(*) FROM slot_runs WHERE need = ?1",
    samples: &[Sample::Text("a1f3c2d0")],
};

/// A need's runs, newest first, at most `?2`: the columns of a run, then why
/// it landed as it did.
pub static NEED_ATTEMPTS: Query = Query {
    name: "need_attempts",
    sql: concat!(
        "SELECT ",
        run_columns!(),
        ", landing_reason FROM slot_runs WHERE need = ?1 \
          ORDER BY dispatched_at DESC LIMIT ?2"
    ),
    samples: &[Sample::Text("a1f3c2d0"), Sample::Int(100)],
};

/// When the run for job `?1` was dispatched.
pub static RUN_DISPATCHED: Query = Query {
    name: "run_dispatched",
    sql: "SELECT dispatched_at FROM slot_runs WHERE job = ?1",
    samples: &[Sample::Text("job-0")],
};

// ---- The need page's flow tab: the pump store (pump.db) ----

/// The columns of one flow run as [`crate::data::need_flow::RunInfo`] reads
/// them, in order.
macro_rules! flow_run_columns {
    () => {
        "run, flow, flow_version, kind, entry, state, state_at"
    };
}

/// The newest flow run of the need `?1`.
pub static FLOW_RUN_LATEST: Query = Query {
    name: "flow_run_latest",
    sql: concat!(
        "SELECT ",
        flow_run_columns!(),
        " FROM flow_runs WHERE need = ?1 ORDER BY run DESC LIMIT 1"
    ),
    samples: &[Sample::Text("a1f3c2d0")],
};

/// Every flow run of the need `?1`, oldest first, at most `?2`.
pub static FLOW_RUNS: Query = Query {
    name: "flow_runs",
    sql: concat!(
        "SELECT ",
        flow_run_columns!(),
        " FROM flow_runs WHERE need = ?1 ORDER BY run LIMIT ?2"
    ),
    samples: &[Sample::Text("a1f3c2d0"), Sample::Int(50)],
};

/// The newest step of run `?2` of the need `?1` that is a flow step: not the
/// run's sentinel and not a slice. The columns are node, task, state, verdict.
/// The run index is named: ordering by rowid otherwise lets the planner walk
/// the whole table when its statistics make `need` look unselective.
pub static FLOW_STEP_CURRENT: Query = Query {
    name: "flow_step_current",
    sql: "SELECT node, task, state, verdict FROM flow_steps INDEXED BY flow_steps_run \
          WHERE need = ?1 AND run = ?2 AND node != 'sentinel' AND node NOT LIKE 'slice:%' \
          ORDER BY rowid DESC LIMIT 1",
    samples: &[Sample::Text("a1f3c2d0"), Sample::Int(1)],
};

/// The steps of run `?2` of the need `?1` in the order they were created,
/// at most `?3`. The columns are node, task, state, verdict.
/// The run index is named: ordering by rowid otherwise lets the planner walk
/// the whole table when its statistics make `need` look unselective.
pub static FLOW_STEPS: Query = Query {
    name: "flow_steps",
    sql: "SELECT node, task, state, verdict FROM flow_steps INDEXED BY flow_steps_run \
          WHERE need = ?1 AND run = ?2 ORDER BY rowid LIMIT ?3",
    samples: &[Sample::Text("a1f3c2d0"), Sample::Int(1), Sample::Int(500)],
};

/// The newest job attempts of the step task `?1`, at most `?2`: job, attempt.
pub static STEP_JOBS_OF_TASK: Query = Query {
    name: "step_jobs_of_task",
    sql: "SELECT job, attempt FROM step_jobs WHERE task = ?1 \
          ORDER BY attempt DESC LIMIT ?2",
    samples: &[Sample::Text("task"), Sample::Int(20)],
};

/// The slice step tasks of run `?2` of the need `?1` with their write scope
/// and fanout link, in creation order, at most `?3`. The columns are task,
/// node, state, verdict, write scope (JSON), scope requests, scope state,
/// scope holders (JSON), fanout task, dependencies (JSON).
pub static SLICE_ROWS: Query = Query {
    name: "slice_rows",
    sql: "SELECT s.task, s.node, s.state, s.verdict, sc.write_scope_json, \
                 sc.scope_requests, sc.scope_state, sc.holders, l.fanout, l.deps_json \
          FROM flow_steps s \
          LEFT JOIN slice_scope sc ON sc.task = s.task \
          LEFT JOIN slice_links l ON l.task = s.task \
          WHERE s.need = ?1 AND s.run = ?2 AND s.node LIKE 'slice:%' \
          ORDER BY s.rowid LIMIT ?3",
    samples: &[Sample::Text("a1f3c2d0"), Sample::Int(1), Sample::Int(300)],
};

/// The newest rung runs of the step task `?1`, at most `?2`: rung, start,
/// finish and exit code.
pub static RUNG_RUNS_OF_TASK: Query = Query {
    name: "rung_runs_of_task",
    sql: "SELECT rung, started_at, finished_at, rc FROM rung_runs \
          WHERE task = ?1 ORDER BY started_at DESC LIMIT ?2",
    samples: &[Sample::Text("task"), Sample::Int(3)],
};

/// The fanout step task open in run `?2` of the need `?1`.
pub static FANOUT_OPEN: Query = Query {
    name: "fanout_open",
    sql: "SELECT fanout FROM fanout_open WHERE need = ?1 AND run = ?2",
    samples: &[Sample::Text("a1f3c2d0"), Sample::Int(1)],
};

/// The commit the fanout step task `?1` started from.
pub static FANOUT_START: Query = Query {
    name: "fanout_start",
    sql: "SELECT tip FROM fanout_starts WHERE fanout = ?1",
    samples: &[Sample::Text("task")],
};

/// The plan revisions of the need `?1`, oldest first, at most `?2`: revision,
/// verdict, reason, time.
pub static PLAN_REVISIONS: Query = Query {
    name: "plan_revisions",
    sql: "SELECT revision, verdict, reason, created_at FROM plan_revisions \
          WHERE need = ?1 ORDER BY revision LIMIT ?2",
    samples: &[Sample::Text("a1f3c2d0"), Sample::Int(50)],
};

/// The need branch of the need `?1`: branch, base commit, status.
pub static NEED_BRANCH: Query = Query {
    name: "need_branch",
    sql: "SELECT branch, base, status FROM need_branches WHERE need = ?1",
    samples: &[Sample::Text("a1f3c2d0")],
};

/// The entry node each restart of the need `?1` started its run at, oldest
/// first, at most `?2`.
pub static RESTART_ENTRIES: Query = Query {
    name: "restart_entries",
    sql: "SELECT run, entry FROM restart_entries WHERE need = ?1 ORDER BY run LIMIT ?2",
    samples: &[Sample::Text("a1f3c2d0"), Sample::Int(50)],
};

/// Every query the console runs against the pump store (`pump.db`). The plan
/// tests (`no_pump_query_scans_a_pump_table`, `pump_plans_never_scan`) seed a
/// pump store for these.
pub static PUMP_REGISTRY: &[&Query] = &[
    &FLOW_RUN_LATEST,
    &FLOW_RUNS,
    &FLOW_STEP_CURRENT,
    &FLOW_STEPS,
    &STEP_JOBS_OF_TASK,
    &SLICE_ROWS,
    &RUNG_RUNS_OF_TASK,
    &FANOUT_OPEN,
    &FANOUT_START,
    &PLAN_REVISIONS,
    &NEED_BRANCH,
    &RESTART_ENTRIES,
    &PUMP_CONTROL,
    &PUMP_UNIT_COUNTS,
    &PUMP_UNIT_EVENTS,
    &PUMP_GATES,
    &PUMP_GATE_FAILURES,
    &PUMP_EXCEPTIONS,
    &PUMP_RECENT_STEP_TASKS,
    &PUMP_RUNG_RUNS,
];

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
    &NEEDS_LIST,
    &NEED_NODE,
    &NEED_MESSAGE,
    &LETTER_OF_CONTENT,
    &THREAD_OF_CONTENT,
    &NEED_ATTEMPT_COUNT,
    &NEED_ATTEMPTS,
    &RUN_DISPATCHED,
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

#[cfg(test)]
#[path = "queries_test.rs"]
mod tests;
