//! Save, reopen and compare round trip of the whole flow index.
//!
//! The flow tables are write-through SQL. This test writes one of every flow
//! record through the public API, reads every table into a value, drops the
//! store, reopens the file and reads again, then reopens once more after
//! writing nothing: what one store wrote a fresh open reads back unchanged,
//! and an open rewrites nothing.
//!
//! Left out on purpose: the pool job store still rides the v8 `meta.jobs`
//! `State` path, so its round trip belongs to the v9 migration (N8). The
//! `rung_runs` table has no writer in the store yet and is left out too.

use std::path::Path;

use rusqlite::types::ValueRef;
use tempfile::tempdir;

use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState, run_state};
use crate::pump::flow::scope::ScopeState;
use crate::pump::store::flow_links::SliceLink;
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

const NEEDS: [&str; 2] = ["n1", "n2"];
const RUNS: [u32; 2] = [1, 2];
const TASKS: [&str; 7] = [
    "t-plan", "t-slice", "t-s1", "t-s2", "t-merge", "t-park", "t-bad",
];
const JOBS: [&str; 4] = ["j-a1", "j-a1-c1", "j-a2", "j-other"];
const FLOW_TABLES: [&str; 12] = [
    "flow_runs",
    "flow_steps",
    "step_jobs",
    "slice_scope",
    "need_branches",
    "applied_actions",
    "main_state",
    "fanout_open",
    "slice_links",
    "restart_entries",
    "fanout_starts",
    "step_payloads",
];
const MAIN_KEYS: [&str; 3] = ["tip", "pushed", "gone"];
const ACTIONS: [&str; 3] = ["act-1", "act-2", "act-3"];

type ScopeRead = (Option<Vec<String>>, Option<ScopeState>, Vec<String>);

/// Every flow table, read through the API and as raw rows.
#[derive(Debug, PartialEq)]
struct Snapshot {
    runs: Vec<Vec<RunRow>>,
    run_times: Vec<Option<i64>>,
    steps: Vec<Vec<StepRow>>,
    steps_by_task: Vec<Option<StepRow>>,
    jobs: Vec<Option<StepJobRow>>,
    attempts: Vec<(u32, u32)>,
    scopes: Vec<ScopeRead>,
    open_fanouts: Vec<Option<String>>,
    links: Vec<Vec<SliceLink>>,
    link_by_task: Vec<Option<SliceLink>>,
    restart_entries: Vec<Option<String>>,
    fanout_starts: Vec<Option<Option<String>>>,
    branches: Vec<Option<NeedBranchRow>>,
    payloads: Vec<Vec<StepPayloadRow>>,
    main_state: Vec<Option<String>>,
    actions: Vec<String>,
    raw: Vec<(String, Vec<Vec<String>>)>,
}

fn s(text: &str) -> String {
    text.to_string()
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|i| s(i)).collect()
}

fn run_row(need: &str, run: u32, kind: &str, entry: &str) -> RunRow {
    RunRow {
        need: s(need),
        run,
        flow: s(kind),
        flow_version: 2,
        kind: s(kind),
        entry: s(entry),
        state: s(run_state::LIVE),
    }
}

fn payload(
    run: &RunRow,
    node: &str,
    task: &str,
    class: &str,
    body: serde_json::Value,
) -> StepPayloadRow {
    StepPayloadRow {
        need: run.need.clone(),
        run: run.run,
        node: s(node),
        class: s(class),
        task: s(task),
        payload: body,
    }
}

fn write_runs_and_steps(store: &PumpStore) -> (RunRow, RunRow) {
    let old = run_row("n1", 1, "build", "plan");
    let live = run_row("n1", 2, "build", "plan");
    let other = run_row("n2", 1, "research", "research");
    for row in [&old, &live, &other] {
        store.insert_run(row).expect("insert run");
    }
    store
        .set_run_state("n1", 1, run_state::SUPERSEDED)
        .expect("supersede run 1");

    let states = [
        ("plan", "t-plan", StepState::Done, Some("approve")),
        ("slice", "t-slice", StepState::Running, None),
        ("slice:s1", "t-s1", StepState::Pending, None),
        ("slice:s2", "t-s2", StepState::Failed, Some("failed")),
        ("merge", "t-merge", StepState::Running, None),
        ("park", "t-park", StepState::Waiting, Some("operator-hold")),
    ];
    for (node, task, state, verdict) in states {
        let mut step = live.step(node, task, state);
        step.verdict = verdict.map(s);
        store.insert_run_step(&live, &step).expect("insert step");
    }
    store
        .insert_run_step(&old, &old.step("plan", "t-bad", StepState::Done))
        .expect("old step");
    store
        .set_step_state("t-bad", StepState::Failed, Some("rejected"))
        .expect("set step state");
    store
        .insert_run_step(
            &other,
            &other.step("research", "t-other", StepState::Pending),
        )
        .expect("other step");
    (live, other)
}

fn write_jobs(store: &PumpStore, live: &RunRow) {
    let step = live.step("slice:s1", "t-s1", StepState::Pending);
    let first = step.job("j-a1", 1);
    let correction = first.again("j-a1-c1");
    let second = first.next("j-a2");
    for row in [&first, &correction, &second] {
        store.insert_step_job("n1", row).expect("insert job");
    }
    let other = live
        .step("merge", "t-merge", StepState::Running)
        .job("j-other", 1);
    store.insert_step_job("n1", &other).expect("insert job");
}

fn write_scopes_and_links(store: &PumpStore, live: &RunRow) {
    store
        .insert_slice_scope("t-s1", 2, &strings(&["src/a.rs", "src/a_test.rs"]))
        .expect("scope s1");
    store
        .insert_slice_scope("t-s2", 2, &strings(&["src/b.rs"]))
        .expect("scope s2");
    store
        .widen_slice_scope("t-s1", &strings(&["src/a.rs", "src/c.rs"]))
        .expect("widen");
    store.bump_scope_requests("t-s1").expect("bump");
    store.bump_scope_requests("t-s1").expect("bump");
    store
        .set_scope_state("t-s1", ScopeState::Wait)
        .expect("scope state");
    store
        .set_scope_holders("t-s1", &strings(&["t-s2"]))
        .expect("holders");
    store
        .set_scope_state("t-s2", ScopeState::Rework)
        .expect("scope state");

    store
        .set_open_fanout("n1", 2, "t-slice")
        .expect("open fanout");
    store
        .link_slice(&SliceLink::for_run(
            live,
            "t-s1",
            Some(s("t-slice")),
            vec![],
        ))
        .expect("link s1");
    store
        .link_slice(&SliceLink::for_run(
            live,
            "t-s2",
            Some(s("t-slice")),
            strings(&["t-s1"]),
        ))
        .expect("link s2");
    store
        .set_slice_deps("t-s2", &strings(&["t-s1", "t-plan"]))
        .expect("deps");
    store
        .set_fanout_start("t-slice", Some("abc123"))
        .expect("fanout start");
    store.set_fanout_start("t-bare", None).expect("bare start");
    store
        .set_restart_entry("n1", 1, "slice")
        .expect("restart entry");
}

fn write_branches_payloads_and_state(store: &PumpStore, live: &RunRow, other: &RunRow) {
    store
        .upsert_need_branch(&NeedBranchRow::for_run(
            live,
            s("vivi/need/n1-r2"),
            Some(s("deadbeef")),
            "live",
            "vivi",
            strings(&["vivi/need/n1", "vivi/need/n1-r1"]),
        ))
        .expect("branch n1");
    store
        .upsert_need_branch(&NeedBranchRow::for_run(
            other,
            s("vivi/need/n2"),
            None,
            "fresh",
            "vivi",
            vec![],
        ))
        .expect("branch n2");
    store
        .record_step_payload(&payload(
            live,
            "plan",
            "t-plan",
            "plan",
            serde_json::json!({"slices": ["s1", "s2"], "n": 2}),
        ))
        .expect("payload plan");
    store
        .record_step_payload(&payload(
            live,
            "merge",
            "t-merge",
            "merge",
            serde_json::json!({"sha": "f00", "ok": true, "tail": null}),
        ))
        .expect("payload merge");
    store.set_main_state("tip", "abc123").expect("main tip");
    store.set_main_state("pushed", "yes").expect("main pushed");
    store.set_main_state("gone", "x").expect("main gone");
    store.clear_main_state("gone").expect("clear main");
    for (at, id) in (1_700_000_000_u64..).zip(ACTIONS) {
        assert!(store.mark_action_applied(id, at).expect("mark action"));
    }
}

fn write_everything(store: &PumpStore) {
    let (live, other) = write_runs_and_steps(store);
    write_jobs(store, &live);
    write_scopes_and_links(store, &live);
    write_branches_payloads_and_state(store, &live, &other);
}

fn read_scope(store: &PumpStore, task: &str) -> ScopeRead {
    (
        store.slice_scope(task).expect("scope"),
        store.scope_state(task).expect("scope state"),
        store.scope_holders(task).expect("holders"),
    )
}

/// Every row of a flow table as text, in rowid order.
fn dump_table(store: &PumpStore, table: &str) -> Vec<Vec<String>> {
    let mut stmt = store
        .conn
        .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
        .expect("prepare dump");
    let columns = stmt.column_count();
    stmt.query_map([], |row| {
        (0..columns)
            .map(|i| {
                Ok(match row.get_ref(i)? {
                    ValueRef::Null => s("NULL"),
                    ValueRef::Integer(n) => n.to_string(),
                    ValueRef::Real(f) => f.to_string(),
                    ValueRef::Text(t) => format!("{:?}", String::from_utf8_lossy(t)),
                    ValueRef::Blob(b) => format!("{b:?}"),
                })
            })
            .collect::<rusqlite::Result<Vec<String>>>()
    })
    .expect("query dump")
    .collect::<Result<_, _>>()
    .expect("dump rows")
}

fn snapshot(store: &PumpStore) -> Snapshot {
    let mut runs = Vec::new();
    let mut steps = Vec::new();
    let mut run_times = Vec::new();
    let mut open_fanouts = Vec::new();
    let mut links = Vec::new();
    let mut restart_entries = Vec::new();
    let mut payloads = Vec::new();
    for need in NEEDS {
        runs.push(store.runs_of(need).expect("runs"));
        payloads.push(store.step_payloads_of_need(need).expect("payloads"));
        for run in RUNS {
            steps.push(store.steps_of(need, run).expect("steps"));
            run_times.push(store.run_state_at(need, run).expect("state_at"));
            open_fanouts.push(store.open_fanout(need, run).expect("open fanout"));
            links.push(store.slice_links_of_run(need, run).expect("links"));
            restart_entries.push(store.restart_entry(need, run).expect("restart entry"));
        }
    }
    let attempts = TASKS
        .iter()
        .map(|t| {
            (
                store.last_attempt(t).expect("last"),
                store.next_attempt(t).expect("next"),
            )
        })
        .collect();
    Snapshot {
        runs,
        run_times,
        steps,
        steps_by_task: TASKS
            .iter()
            .map(|t| store.step_by_task(t).expect("by task"))
            .collect(),
        jobs: JOBS
            .iter()
            .map(|j| store.step_job(j).expect("job"))
            .collect(),
        attempts,
        scopes: ["t-s1", "t-s2", "t-none"]
            .iter()
            .map(|t| read_scope(store, t))
            .collect(),
        open_fanouts,
        links,
        link_by_task: ["t-s1", "t-s2", "t-none"]
            .iter()
            .map(|t| store.slice_link(t).expect("link"))
            .collect(),
        restart_entries,
        fanout_starts: ["t-slice", "t-bare", "t-none"]
            .iter()
            .map(|f| store.fanout_start(f).expect("fanout start"))
            .collect(),
        branches: NEEDS
            .iter()
            .map(|n| store.need_branch(n).expect("branch"))
            .collect(),
        payloads,
        main_state: MAIN_KEYS
            .iter()
            .map(|k| store.main_state(k).expect("main state"))
            .collect(),
        actions: store
            .applied_action_ids()
            .expect("actions")
            .into_iter()
            .collect(),
        raw: FLOW_TABLES
            .iter()
            .map(|t| (s(t), dump_table(store, t)))
            .collect(),
    }
}

fn open(path: &Path) -> PumpStore {
    PumpStore::open(path).expect("open")
}

#[test]
fn the_whole_flow_index_survives_a_reopen_unchanged() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join(DB_FILENAME);

    let written = {
        let store = open(&path);
        write_everything(&store);
        snapshot(&store)
    };

    // The fixture is not vacuous: every table holds rows, and the interesting
    // values are really there.
    for (table, rows) in &written.raw {
        assert!(!rows.is_empty(), "table {table} has no rows");
    }
    assert_eq!(written.attempts[2], (2, 3), "attempts of t-s1");
    assert_eq!(
        written.main_state,
        [Some(s("abc123")), Some(s("yes")), None]
    );
    assert_eq!(written.actions, ["act-1", "act-2", "act-3"]);
    assert_eq!(written.fanout_starts[1], Some(None));

    let reopened = snapshot(&open(&path));
    assert_eq!(reopened, written, "first reopen");

    let again = snapshot(&open(&path));
    assert_eq!(again, written, "second reopen after writing nothing");
}
