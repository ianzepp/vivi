//! Exhausted-slice attempt diagnostics through the real mailspace: the
//! attempt record the pump stored on the slice task (not invented data) is
//! what the head-cto brief carries, and a slice task whose thread cannot be
//! read fails the cto start instead of passing silently.

use std::fs;
use std::sync::Arc;

use super::CtoStart;
use crate::error::VivariumError;
use crate::letter::fixture::task_body_with;
use crate::letter::records::NoteWord;
use crate::letter::work::TaskFront;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailspacePort;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::slice_dispatch::{MAX_ATTEMPTS, Retried, SliceMail, retry};
use crate::pump::host::{Ctx, FakeHost};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob};
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};

type R<T> = Result<T, VivariumError>;

/// The pump identity of the default config's `reply_from`.
const PUMP: &str = "mind";

/// The pool receipt of the failed last attempt: a nonzero agent exit, a short
/// provider-error excerpt (status 429, code 1302) and the hand's own final
/// message.
const RECEIPT: &str = concat!(
    "pool result j1: finished\n",
    "pool p  slot s1  model m\n",
    "from next  default\n",
    "push: none\n",
    "agent exit 1 in 12s, job 20s, started from 1234567890\n",
    "\n--- receipt\n",
    "PROVIDER REFUSED THE ATTEMPT; no changes were made\n",
    "\n--- stderr tail\n",
    "post to provider failed: status 429\n",
    "upstream error code 1302: rate limited\n",
    "\n--- runner log tail\n",
    "log line",
);

/// A real mailspace project with its pump context, fake host, index and port.
struct Rig {
    _project: Project,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
    port: MailspacePort,
}

fn rig() -> R<Rig> {
    let p = project();
    let ctx = p.pump_ctx();
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let port = MailspacePort {
        mailspace: p.mailspace.clone(),
        pump_identity: PUMP.into(),
    };
    Ok(Rig {
        host: p.host.clone(),
        _project: p,
        ctx,
        store,
        port,
    })
}

/// A build run of `need`, live at its cto gate.
fn build_run(need: &str, number: u32) -> RunRow {
    RunRow {
        need: need.into(),
        run: number,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "slice".into(),
        state: "live".into(),
    }
}

/// The failed last-attempt hand job on `task`: its task and attempt payload
/// match the exhausted slice, its outcome carries the provider receipt.
fn failed_job(task: &str) -> PoolJob {
    let mut spec = JobSpec::new(
        format!("slice-{task}-a{MAX_ATTEMPTS}"),
        "hand",
        "hand",
        "s",
        "b",
    );
    spec.task = Some(task.to_owned());
    spec.branch = Some(format!("vivi/unit/{task}-a{MAX_ATTEMPTS}"));
    let mut job = PoolJob::from(spec);
    job.status = JobStatus::Finished;
    job.payload = serde_json::json!({"attempt": MAX_ATTEMPTS});
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: RECEIPT.into(),
        at: 0,
    });
    job
}

/// FakeHost records the pump's outgoing `mail reply` without delivering it:
/// find the attempt body file the record wrote under `board`, check the
/// command targeted the slice task with that exact file, then store the
/// file's content on the task as the record's delivery.
fn deliver_recorded_attempt(rig: &Rig, task: &str) -> R<()> {
    let board = rig.ctx.home.join("board");
    let prefix = format!("reply-{task}-attempt-");
    let path = fs::read_dir(&board)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .unwrap_or_else(|| panic!("no outgoing attempt body file in {}", board.display()));
    let file = path.to_string_lossy().into_owned();
    assert!(
        rig.host
            .commands()
            .iter()
            .any(|cmd| cmd.contains(&format!("mail reply {task} ")) && cmd.contains(&file)),
        "the mail-reply command must target the slice task with {file}"
    );
    let body = fs::read_to_string(&path)?;
    rig.port.reply(task, &body)?;
    Ok(())
}

/// How starting the cto step of `run` over the real port comes out.
fn start_cto(rig: &Rig, run: &RunRow) -> R<StartAction> {
    let def = load(FlowKind::Build)?;
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.port,
        run,
        def: &def,
    };
    let step = StepRow {
        run: run.run,
        node: "cto".into(),
        task: "t-cto".into(),
        state: StepState::Waiting,
        verdict: None,
    };
    CtoStart.start(&cx, &step)
}

/// The job of [`start_cto`], unwrapped.
fn cto_job(rig: &Rig, run: &RunRow) -> R<JobSpec> {
    match start_cto(rig, run)? {
        StartAction::Job(spec) => Ok(*spec),
        other => panic!("expected a job, got {other:?}"),
    }
}

#[test]
fn failed_slice_attempt_excerpt_reaches_the_cto_brief() -> R<()> {
    let rig = rig()?;
    let need = "need-diag";
    let run = build_run(need, 2);
    rig.store.insert_run(&run)?;

    let task = rig.port.send_step(
        PUMP,
        PUMP,
        "slice diagnose",
        &task_body_with(
            &TaskFront {
                target: Some("vivi".into()),
                ..TaskFront::step(need, 2, "slice:a")
            },
            "",
        ),
        &[],
    )?;
    let slice = StepRow {
        run: 2,
        node: "slice:a".into(),
        task: task.clone(),
        state: StepState::Running,
        verdict: None,
    };
    rig.store.insert_step(need, &slice)?;

    let mut st = State::default();
    let retried = retry(
        &rig.ctx,
        &mut st,
        &rig.store,
        &rig.port,
        need,
        &slice,
        &failed_job(&task),
        MAX_ATTEMPTS,
    )?;
    assert_eq!(retried, Retried::Exhausted);
    let indexed = rig.store.step_by_task(&task)?.expect("indexed slice");
    assert_eq!(indexed.state, StepState::Failed);
    assert_eq!(indexed.verdict.as_deref(), Some("failed"));

    deliver_recorded_attempt(&rig, &task)?;
    let stored = rig.port.notes(&task, NoteWord::Attempt)?;
    assert_eq!(stored.len(), 1, "one attempt record on the task");

    let spec = cto_job(&rig, &run)?;
    assert_eq!(spec.role, "head-cto");
    assert!(spec.read_only);
    assert_eq!(spec.kind, "flow-cto");
    let head = format!("run 2 slice:a (task {task}, outcome agent failed: hand failed):");
    assert!(spec.body.contains(&head), "{}", spec.body);
    assert!(spec.body.contains("pump: attempt record"), "{}", spec.body);
    assert!(
        spec.body.contains(concat!(
            "failing command:\n",
            "  agent exit 1\n",
            "excerpt:\n",
            "  post to provider failed: status 429\n",
            "  upstream error code 1302: rate limited\n",
        )),
        "{}",
        spec.body
    );
    assert!(
        spec.body.contains(concat!(
            "hand's final message:\n",
            "  PROVIDER REFUSED THE ATTEMPT; no changes were made\n",
        )),
        "{}",
        spec.body
    );
    Ok(())
}

#[test]
fn attempt_thread_read_errors_are_not_hidden() -> R<()> {
    let rig = rig()?;
    let need = "need-broken-thread";
    let early = build_run(need, 1);
    let late = build_run(need, 2);
    rig.store.insert_run(&early)?;
    rig.store.insert_run(&late)?;
    rig.store.insert_step(
        need,
        &StepRow {
            run: 1,
            node: "slice:ghost".into(),
            task: "t-never-sent".into(),
            state: StepState::Failed,
            verdict: Some("failed".into()),
        },
    )?;

    let Err(error) = start_cto(&rig, &late) else {
        panic!("a slice task whose thread cannot be read must fail the start");
    };
    assert!(
        error.to_string().contains("message not found"),
        "expected the missing-thread error, got {error}"
    );
    Ok(())
}
