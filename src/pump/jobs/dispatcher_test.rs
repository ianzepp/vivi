//! Tests for the dispatcher's durable dispatch evidence: the start record
//! written before a pool is asked, the accepted association persisted before
//! the job map save, and the holds an unresolved ownership takes.

use std::collections::BTreeMap;
use std::fs;

use super::{dispatch_pending, evidence};
use crate::letter::work::JobFront;
use crate::mailspace::SendRequest;
use crate::pool::collect::{Collected, JobResult, Outcome};
use crate::pool::record::{DispatchRecord, OpenDispatch, collected_note, dispatch_note};
use crate::pump::fixture::{Project, project};
use crate::pump::host::FakeHost;
use crate::pump::jobs::execution_receipt::{self, ExecutionQuery, ExecutionReceipt};
use crate::pump::jobs::{JobSpec, JobStatus, JobStore, PoolJob, enqueue};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// The fake clock's start, shared with the fixture.
const T0: u64 = 1_000_000;

#[derive(Default)]
struct Store {
    jobs: BTreeMap<String, PoolJob>,
}

impl JobStore for Store {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.jobs
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.jobs
    }
}

/// A project whose pump home (and `pump.db` inside it) exists, one task in
/// its mailspace, and a `studio` pool with one free slot.
fn rig() -> (Project, String) {
    let p = project();
    p.pump_ctx();
    drop(PumpStore::open(&p.ctx.home.join(DB_FILENAME)).unwrap());
    let task = send_task(&p);
    p.host.reply(
        "pool status",
        0,
        "slot studio s1 slot-1 free - - - - - -\n",
    );
    (p, task)
}

fn send_task(p: &Project) -> String {
    let sent = p
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["elsewhere".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Fix the edge".into(),
            body: crate::letter::fixture::task_body("do it\n"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap();
    sent.delivered[0].handle.clone()
}

fn spec(id: &str, task: &str) -> JobSpec {
    let mut spec = JobSpec::new(id, "merge", "merge", "integrate", "merge branches");
    spec.pool_candidates = vec![String::from("studio")];
    spec.task = Some(task.to_owned());
    spec
}

fn pool_store(p: &Project) -> PumpStore {
    PumpStore::open(&p.ctx.home.join(DB_FILENAME)).unwrap()
}

fn record(pool_job: &str, slot: &str, at: u64) -> DispatchRecord {
    DispatchRecord {
        target: None,
        repository: None,
        pool: "studio".into(),
        need: None,
        kind: None,
        slot: slot.into(),
        machine: "m1".into(),
        job: pool_job.into(),
        model: "m/x".into(),
        from: "main".into(),
        branch: "factory/a".into(),
        start: "a".repeat(40),
        with: Vec::new(),
        budget: 600,
        at,
        read_only: false,
        hardware: String::new(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    }
}

/// Record the pool's own `pool_dispatched` event on the task thread.
fn dispatch_event(p: &Project, task: &str, pool_job: &str, at: u64) {
    p.mailspace
        .record_pool_note(
            task,
            "pool_dispatched",
            &dispatch_note(&record(pool_job, "slot-1", at)),
            "dispatched",
        )
        .unwrap();
}

/// Record the pool's own `pool_collected` event closing `pool_job`.
fn collected_event(p: &Project, task: &str, pool_job: &str) {
    let open = OpenDispatch {
        task: task.to_owned(),
        record: record(pool_job, "slot-1", 0),
    };
    let finished = Outcome::Finished(Collected {
        result: JobResult {
            fields: BTreeMap::new(),
        },
        receipt: String::new(),
        stderr: String::new(),
        log: String::new(),
    });
    p.mailspace
        .record_pool_note(task, "pool_collected", &collected_note(&open, &finished), "done")
        .unwrap();
}

/// The canned `pool dispatch` answer of a pool that accepted: a full
/// porcelain `dispatched` line naming its own job id.
fn dispatched_reply(p: &Project, task: &str) {
    p.host.reply(
        "pool dispatch",
        0,
        &format!(
            "dispatched {task} studio slot-1 pj-1 m/x main {} - 600 write 0 -\n",
            "a".repeat(40)
        ),
    );
}

/// The canned `mail thread` answer carrying this job's pump brief, so the
/// thread reads as briefed without posting a new note.
fn brief_thread_reply(p: &Project, job: &str) {
    let brief = crate::mailspace::pump_note::brief_body(
        JobFront {
            id: job.to_owned(),
            kind: String::from("merge"),
            builtin_role: None,
        },
        "merge branches",
    )
    .unwrap();
    let thread = serde_json::json!({ "messages": [{ "body": brief }] }).to_string();
    p.host.reply("mail thread", 0, &thread);
}

fn pool_dispatch_calls(p: &Project) -> usize {
    p.host
        .commands()
        .iter()
        .filter(|command| command.contains("pool dispatch"))
        .count()
}

fn query<'a>(
    p: &'a Project,
    job: &'a str,
    task: &'a str,
    events: &'a [crate::mailspace::PoolEventRow],
    prior: &'a [String],
) -> ExecutionQuery<'a> {
    ExecutionQuery {
        job,
        task,
        events,
        briefed: false,
        prior_pool_jobs: prior,
    }
}

#[test]
fn the_dispatched_line_is_parsed_shell_aware() {
    let line = "dispatched \"my task\" studio slot-1 pj-9 m/x main x - 600 write 0 -\n";
    let accepted = evidence::accepted_line(line).unwrap();
    assert_eq!(accepted.task, "my task");
    assert_eq!(accepted.pool, "studio");
    assert_eq!(accepted.slot, "slot-1");
    assert_eq!(accepted.pool_job, "pj-9");
    assert!(evidence::accepted_line("remote refused\n").is_none());
    assert!(evidence::accepted_line("dispatched t studio\n").is_none());
    assert!(evidence::accepted_line("").is_none());
}

#[test]
fn an_accepted_association_survives_an_unsaved_job_map() {
    let (p, task) = rig();
    dispatched_reply(&p, &task);
    let mut store = Store::default();
    enqueue(&mut store, spec("m1", &task)).unwrap();
    assert_eq!(dispatch_pending(&p.ctx, &mut store).unwrap(), ["m1"]);
    assert_eq!(store.jobs["m1"].status, JobStatus::Dispatched);

    // The association is durable in pump.db, before any meta.jobs save.
    let saved = execution_receipt::load(&pool_store(&p), "m1").unwrap();
    let accepted = saved.accepted.expect("the accepted association");
    assert_eq!(accepted.task.as_deref(), Some(task.as_str()));
    assert_eq!(accepted.pool.as_deref(), Some("studio"));
    assert_eq!(accepted.slot.as_deref(), Some("slot-1"));
    assert_eq!(accepted.pool_job.as_deref(), Some("pj-1"));
    assert_eq!(saved.started_at, Some(T0));

    // With the job map lost, the saved association alone reconstructs the
    // execution's dispatch from the task's typed notes.
    dispatch_event(&p, &task, "pj-1", T0 + 5);
    let events = p.mailspace.pool_events().unwrap();
    let receipt = execution_receipt::receipt(&pool_store(&p), query(&p, "m1", &task, &events, &[]))
        .unwrap();
    let ExecutionReceipt::Dispatched(selected) = receipt else {
        panic!("expected the dispatch, got {receipt:?}");
    };
    assert_eq!(selected.pool_job, "pj-1");
    assert_eq!(selected.pool, "studio");
    assert_eq!(selected.slot, "slot-1");
    assert_eq!(selected.task, task);
}

#[test]
fn a_crash_after_acceptance_reconstructs_on_the_next_dispatch() {
    let (p, task) = rig();
    dispatched_reply(&p, &task);
    let mut store = Store::default();
    enqueue(&mut store, spec("m1", &task)).unwrap();
    dispatch_pending(&p.ctx, &mut store).unwrap();
    dispatch_event(&p, &task, "pj-1", T0 + 5);

    // The pump restarts: the acceptance outlived the save, the job map did
    // not, so the job is pending again. The re-dispatch answer is lost.
    let mut store = Store::default();
    enqueue(&mut store, spec("m1", &task)).unwrap();
    p.host.reply("pool dispatch", 124, "");
    assert_eq!(dispatch_pending(&p.ctx, &mut store).unwrap(), ["m1"]);

    let job = &store.jobs["m1"];
    assert_eq!(job.status, JobStatus::Dispatched);
    assert_eq!(job.pool.as_deref(), Some("studio"));
    assert_eq!(job.dispatched_at, Some(T0));
    let saved = execution_receipt::load(&pool_store(&p), "m1").unwrap();
    assert_eq!(
        saved.accepted.and_then(|accepted| accepted.pool_job),
        Some(String::from("pj-1"))
    );
}

#[test]
fn an_uncertain_attempt_with_unresolved_ownership_holds() {
    let (p, task) = rig();
    p.host.reply("pool dispatch", 124, "");
    brief_thread_reply(&p, "m1");
    let mut store = Store::default();
    enqueue(&mut store, spec("m1", &task)).unwrap();

    // The first attempt only records its start; nothing is on the thread.
    dispatch_pending(&p.ctx, &mut store).unwrap();
    assert!(store.jobs["m1"].held.is_none());

    // Two dispatches then follow the start, both candidates for the job.
    dispatch_event(&p, &task, "pj-1", T0 + 5);
    dispatch_event(&p, &task, "pj-2", T0 + 10);
    dispatch_pending(&p.ctx, &mut store).unwrap();

    let job = &store.jobs["m1"];
    assert_eq!(job.status, JobStatus::Pending);
    assert!(job.held.is_some(), "the job is held");
    let exceptions = fs::read_to_string(p.ctx.home.join("exceptions.txt")).unwrap();
    assert!(
        exceptions.contains("flow-job-receipt-ambiguous m1"),
        "{exceptions}"
    );
    let calls = pool_dispatch_calls(&p);
    dispatch_pending(&p.ctx, &mut store).unwrap();
    assert_eq!(pool_dispatch_calls(&p), calls, "a held job is not offered");
}

#[test]
fn a_definite_refusal_adopts_no_old_task_result() {
    let (p, task) = rig();
    // A prior execution of this task dispatched and finished long ago.
    dispatch_event(&p, &task, "pj-old", T0 - 1_000);
    collected_event(&p, &task, "pj-old");
    p.host.reply("pool dispatch", 1, "refused t \"no usable bridge\"\n");
    let mut store = Store::default();
    enqueue(&mut store, spec("m1", &task)).unwrap();

    assert!(dispatch_pending(&p.ctx, &mut store).unwrap().is_empty());

    let job = &store.jobs["m1"];
    assert_eq!(job.status, JobStatus::Pending);
    assert_eq!(job.dispatch_refusals, 1);
    assert!(job.outcome.is_none(), "the old result is not adopted");
    assert_eq!(job.pool, None);
    let exceptions = fs::read_to_string(p.ctx.home.join("exceptions.txt")).unwrap_or_default();
    assert!(!exceptions.contains("flow-job-receipt-ambiguous"));
    // The attempt still left its start evidence behind.
    let saved = execution_receipt::load(&pool_store(&p), "m1").unwrap();
    assert!(saved.started_at.is_some());
    assert!(saved.accepted.is_none());
}
