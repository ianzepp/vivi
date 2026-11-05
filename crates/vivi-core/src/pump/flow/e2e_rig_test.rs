//! End to end: the shared rig for the end to end flow tests.
//!
//! A [`Rig`] is a real mailspace project, a pump context under a caller
//! config, the shipped step handlers, and an in-memory job table the test
//! drives as the pool would: it ticks the pump, finds the job a step opened,
//! finishes it with a scripted receipt, and ticks again. The tests of the
//! research, release and repair flows build on it.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::pump::config::PumpConfig;
use crate::pump::fixture::{Project, project, send_need};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::handlers::{Registry, shipped};
use crate::pump::flow::index::{RunRow, StepRow};
use crate::pump::flow::mail::{MailReader, MailWriter, MailspacePort, NeedStatus};
use crate::pump::flow::main_rung::{PAYLOAD_KEY, STARTED, TASK, TESTED, key};
use crate::pump::flow::tick::tick_with;
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob, rung};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;
use vivi_error::VivariumError;
use vivi_mail::letter::read::record_of;
use vivi_mail::letter::records::RecordKind;
use vivi_mail::letter::work::TaskFront;
use vivi_mail::letter::{fixture, render};

type R = Result<(), VivariumError>;

/// The pump's mailspace identity: the default `[pump] reply_from`.
const PUMP: &str = "mind";
/// Ticks `tick_until` may take before it gives up.
const MAX_TICKS: usize = 12;
/// The pool the rig's routed roles run on (the fixture's own pool).
const POOL: &str = "test-planner-pool";

/// The jobs the pump enqueued, in memory.
#[derive(Default)]
pub(crate) struct Jobs {
    pub jobs: BTreeMap<String, PoolJob>,
}

impl JobStore for Jobs {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.jobs
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.jobs
    }
}

/// One need with an open pump record and a started run, over a real
/// mailspace, driven by the real tick.
pub(crate) struct Rig {
    pub project: Project,
    pub ctx: Ctx,
    pub store: PumpStore,
    pub port: MailspacePort,
    pub jobs: Jobs,
    pub registry: Registry<Jobs, MailspacePort>,
    pub need: String,
}

/// The main rung target both rigs report on.
const TARGET: &str = "default";

fn routed(name: &str, kind: &str) -> RoutedRole {
    RoutedRole {
        name: name.into(),
        kind: Some(kind.into()),
        status: String::from("active"),
        pool: Some(POOL.into()),
        read_only: None,
        model: None,
        models: Vec::new(),
    }
}

/// The shared machinery of both rigs: a real mailspace project under `cfg`,
/// its routed context (`researcher` and `repairer` besides the fixture's
/// planner), an open store and a port onto the project's mailspace. No record
/// is opened and no run is started here.
fn assembly(cfg: PumpConfig) -> Result<(Project, Ctx, PumpStore, MailspacePort), VivariumError> {
    let project = project();
    let mut ctx = project.pump_ctx();
    ctx.cfg = Arc::new(cfg);
    assert_eq!(ctx.cfg.reply_from, PUMP, "the rig speaks as the pump");
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(routed("researcher", "researcher"));
    routing.roles.push(routed("repairer", "repairer"));
    ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let port = MailspacePort {
        mailspace: crate::pump::record::PumpMailspace::new(project.mailspace.clone()),
        pump_identity: PUMP.into(),
    };
    Ok((project, ctx, store, port))
}

/// A rig under `cfg`: a need sent to the pump identity, its pump record
/// opened with `record_body` (a record letter without `need`, for example
/// front matter with `kind = "repair"`, `target` and `tested`), and a run
/// started at `entry`. The routing holds
/// a `researcher` and a `repairer` role besides the fixture's planner.
///
/// # Errors
/// Returns a [`VivariumError`] when the record or the run cannot be opened.
pub(crate) fn rig(cfg: PumpConfig, record_body: &str, entry: &str) -> Result<Rig, VivariumError> {
    let (project, ctx, store, mut port) = assembly(cfg)?;
    let need = project.need("a need for the end to end rig");
    MailWriter::open_record(
        &mut port,
        &need,
        &fixture::record_with_need(record_body, &need),
    )?;
    start(&ctx, &store, &mut port, &need, Some(entry))?;
    let registry = shipped::<Jobs, MailspacePort>(&ctx);
    Ok(Rig {
        project,
        ctx,
        store,
        port,
        jobs: Jobs::default(),
        registry,
        need,
    })
}

/// A rig whose main rung went red and filed its repair need: the same real
/// mailspace, routed context, store and shipped registry as [`rig`], with one
/// saved main rung job carrying `receipt` already in the job table. The job
/// remembers the original command and target, the tested `tested` tip, its
/// own durable id and the target's long-lived task. One tick applies it
/// through the normal main rung phase, so the repair need is sent, its record
/// is opened and its prose is the red filing's own report. The rig returns
/// before `fix1` is queued: its `need` is the filed repair need, and the
/// started and tested state keeps an unrelated main rung from being
/// scheduled. The main source job stays in [`Rig::jobs`].
///
/// The fixture host answers the `need send` transport with a real need it
/// created up front; everything the record says about the failure comes from
/// the filing code.
///
/// # Errors
/// Returns a [`VivariumError`] when the mailspace, the saved job or the tick
/// fails, or when `cfg` configures no main rung for the target.
pub(crate) fn filed_repair_rig(
    cfg: PumpConfig,
    tested: &str,
    receipt: &str,
) -> Result<Rig, VivariumError> {
    let (project, ctx, store, mut port) = assembly(cfg)?;
    let filed = project.need(&format!("pump: main rung red on {TARGET} at {tested}"));
    project
        .host
        .reply("need send", 0, &format!("created need {filed}\n"));
    let task = main_task(&port)?;
    let job = red_job(&ctx, &task, tested, receipt)?;
    store.set_main_state(&key(TASK, TARGET), &task)?;
    store.set_main_state(&key(STARTED, TARGET), &ctx.now().to_string())?;
    store.set_main_state(&key(TESTED, TARGET), tested)?;
    let registry = shipped::<Jobs, MailspacePort>(&ctx);
    let mut jobs = Jobs::default();
    jobs.jobs.insert(job.id.clone(), job);
    let mut rig = Rig {
        project,
        ctx,
        store,
        port,
        jobs,
        registry,
        need: filed,
    };
    rig.tick()?;
    Ok(rig)
}

/// The target's main rung task, sent the way the schedule sends it: pump to
/// pump, a task letter with its own work front.
fn main_task(port: &MailspacePort) -> Result<String, VivariumError> {
    let body = render(
        &TaskFront::default(),
        &format!("pump main rung on {TARGET}"),
    )
    .map_err(|e| VivariumError::Message(e.to_string()))?;
    port.send_step(
        PUMP,
        PUMP,
        &format!("pump: main rung on {TARGET}"),
        &body,
        &[],
    )
}

/// The saved main rung job of a red run: the rung job of a `main` rung at
/// `tested` (durable id, exact command, target and start ref, dispatch task),
/// finished with `receipt` and not yet applied.
fn red_job(ctx: &Ctx, task: &str, tested: &str, receipt: &str) -> Result<PoolJob, VivariumError> {
    let short: String = tested.chars().take(12).collect();
    let step_task = format!("main-{TARGET}-{short}");
    let mut spec = rung::spec(ctx, TARGET, "main", tested, &step_task, 1).ok_or_else(|| {
        VivariumError::Config(format!(
            "filed repair rig: expected a main rung command for target {TARGET}; actual: not configured"
        ))
    })?;
    spec.task = Some(task.to_owned());
    if let Some(payload) = spec.payload.as_object_mut() {
        payload.insert(PAYLOAD_KEY.to_owned(), serde_json::json!(TARGET));
    }
    let mut job = PoolJob::from(spec);
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::new(),
        receipt: receipt.to_owned(),
        at: ctx.now(),
    });
    Ok(job)
}

impl Rig {
    /// One tick of the pump.
    ///
    /// # Errors
    /// Returns the tick's error.
    pub(crate) fn tick(&mut self) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.port,
            &self.registry,
        )
    }

    /// Tick until `pred` holds, at most 12 ticks; the check runs before the
    /// first tick too.
    ///
    /// # Errors
    /// Returns a tick error, or an error naming `label` when `pred` never
    /// held.
    pub(crate) fn tick_until(&mut self, label: &str, pred: impl Fn(&Rig) -> bool) -> R {
        for _ in 0..MAX_TICKS {
            if pred(self) {
                return Ok(());
            }
            self.tick()?;
        }
        if pred(self) {
            return Ok(());
        }
        Err(VivariumError::Message(format!(
            "never reached `{label}` in {MAX_TICKS} ticks: runs {:?}, steps {:?}, exceptions {:?}",
            self.runs()?,
            self.store.steps_of(&self.need, self.latest_run()?),
            self.exceptions()
        )))
    }

    fn latest_run(&self) -> Result<u32, VivariumError> {
        Ok(self.runs()?.iter().map(|run| run.run).max().unwrap_or(1))
    }

    /// The need's runs, oldest first.
    ///
    /// # Errors
    /// Returns a store error.
    pub(crate) fn runs(&self) -> Result<Vec<RunRow>, VivariumError> {
        self.store.runs_of(&self.need)
    }

    /// The newest row of `node` in the newest run.
    ///
    /// # Errors
    /// Returns a store error.
    pub(crate) fn row(&self, node: &str) -> Result<Option<StepRow>, VivariumError> {
        Ok(self
            .store
            .steps_of(&self.need, self.latest_run()?)?
            .into_iter()
            .rev()
            .find(|row| row.node == node))
    }

    /// The newest pending or dispatched job whose payload `node` is `node`.
    #[must_use]
    pub(crate) fn open_job(&self, node: &str) -> Option<PoolJob> {
        self.jobs
            .jobs
            .values()
            .filter(|job| matches!(job.status, JobStatus::Pending | JobStatus::Dispatched))
            .filter(|job| job.payload.get("node").and_then(|n| n.as_str()) == Some(node))
            .max_by_key(|job| job.queued_at)
            .cloned()
    }

    /// End job `id` as the pool would: `status` and an outcome carrying
    /// `receipt`.
    ///
    /// # Errors
    /// Returns an error when no job has that id.
    pub(crate) fn finish(&mut self, id: &str, status: JobStatus, receipt: &str) -> R {
        let known: Vec<String> = self.jobs.jobs.keys().cloned().collect();
        let job = self
            .jobs
            .jobs
            .get_mut(id)
            .ok_or_else(|| VivariumError::Message(format!("no job {id} among {known:?}")))?;
        job.status = status;
        job.outcome = Some(JobOutcome {
            state: match status {
                JobStatus::Timeout => "timeout".into(),
                JobStatus::Lost => "lost".into(),
                _ => "finished".into(),
            },
            detail: String::new(),
            receipt: receipt.into(),
            at: 0,
        });
        Ok(())
    }

    /// Open a second need on this rig: a new need with its own pump record
    /// (`record_body`) and a run started at `entry`. Returns its handle. The
    /// rig's own `need`, `row` and `runs` keep pointing at the first need.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the record or the run cannot be
    /// opened.
    pub(crate) fn add_need(
        &mut self,
        subject: &str,
        record_body: &str,
        entry: &str,
    ) -> Result<String, VivariumError> {
        let need = send_need(&self.port.mailspace.lock(), subject, PUMP);
        MailWriter::open_record(
            &mut self.port,
            &need,
            &vivi_mail::letter::fixture::record_with_need(record_body, &need),
        )?;
        start(&self.ctx, &self.store, &mut self.port, &need, Some(entry))?;
        Ok(need)
    }

    /// The pump's `exceptions.txt`; empty when it was never written.
    #[must_use]
    pub(crate) fn exceptions(&self) -> String {
        match std::fs::read_to_string(self.ctx.home.join("exceptions.txt")) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => format!("exceptions.txt unreadable: {e}"),
        }
    }

    /// Script the `ls-remote origin <reference>` reply: `sha` at `reference`.
    pub(crate) fn git(&self, reference: &str, sha: &str) {
        self.project.host.reply(
            &format!("ls-remote origin {reference}"),
            0,
            &format!("{sha}\t{reference}\n"),
        );
    }
}

/// A pool result for an agent job that ended with `exit`, its last message
/// `message` (the envelope, as the worker wrote it).
#[must_use]
pub(crate) fn agent_receipt(exit: i32, message: &str) -> String {
    push_receipt(exit, "", "", message)
}

/// A pool result whose summary carries a `push:` line and a `runner publish:`
/// line (either may be empty, and then it is left out or blank).
#[must_use]
pub(crate) fn push_receipt(
    exit: i32,
    push_line: &str,
    runner_publish_line: &str,
    message: &str,
) -> String {
    let publish = if runner_publish_line.is_empty() {
        String::new()
    } else {
        format!("{runner_publish_line}\n")
    };
    format!(
        "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
         push: {push_line}\nagent exit {exit} in 12s, job 20s, started from 1234567890\n\
         {publish}\n--- receipt\n{message}\n\n--- stderr tail\n\n\n--- runner log tail\nlog line"
    )
}

/// A pool result of a job whose agent exited 127 because `tool` is missing.
#[must_use]
pub(crate) fn env_receipt(tool: &str) -> String {
    format!(
        "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
         push: \nagent exit 127 in 1s, job 2s, started from 1234567890\n\n\
         --- receipt\n\n\n--- stderr tail\nbash: {tool}: command not found\n\n\
         --- runner log tail\nlog line"
    )
}

/// A main rung receipt: an early size-check error above passing noise and one
/// failing test, so a tail-only report would lose the error.
const RED_RECEIPT: &str = "error: src/pump/flow/main_rung/red.rs has 1004 lines, over the 1000-line ceiling\n\
    Running unittests src/lib.rs (target/debug/deps/vivi_core-1a2b3c4d5e)\n\
test pump::flow::passes ... ok\n\
test pump::flow::breaks ... FAILED\n\
agent exit 101 in 9s\n";

/// The tip the saved main rung job ran at.
const TESTED_TIP: &str = "abcdef0123456789";
/// A newer main tip a caller sets once the record is filed.
const NEWER_TIP: &str = "fedcba9876543210";
/// The canonical failing id the receipt's failing line names.
const FAILING_ID: &str = "vivi_core::pump::flow::breaks";

/// The ids of the jobs the rig holds, oldest first.
fn seeded_job(rig: &Rig) -> Result<PoolJob, VivariumError> {
    rig.jobs
        .jobs
        .values()
        .find(|job| job.kind == "rung")
        .cloned()
        .ok_or_else(|| VivariumError::Message("the rig kept no main rung job".into()))
}

/// The texts a filed record and its fix1 brief must both carry: the saved
/// job's provenance and the early evidence a tail-only report would lose.
fn filed_texts(source: &str) -> Vec<String> {
    vec![
        String::from("command: cargo test --quiet"),
        String::from("target: default"),
        format!("tested: {TESTED_TIP}"),
        format!("source job: {source}"),
        String::from("has 1004 lines"),
        String::from(FAILING_ID),
    ]
}

#[test]
fn a_filed_repair_record_carries_saved_provenance_into_the_fix1_brief() -> R {
    let cfg = PumpConfig {
        rungs: crate::pump::config::RungsConfig {
            main: Some(String::from("cargo test --quiet")),
            ..crate::pump::config::RungsConfig::default()
        },
        ..PumpConfig::default()
    };
    let mut rig = filed_repair_rig(cfg, TESTED_TIP, RED_RECEIPT)?;
    let source = seeded_job(&rig)?.id;
    let record = rig
        .port
        .open_record(&rig.need)?
        .map(|(_handle, body)| body)
        .ok_or_else(|| VivariumError::Message("the rig filed no record".into()))?;
    let front = record_of("", &record)
        .map_err(|e| VivariumError::Message(format!("bad filed record: {e}")))?
        .front;
    assert_eq!(front.kind, Some(RecordKind::Repair));
    assert_eq!(front.target.as_deref(), Some("default"));
    assert_eq!(front.tested.as_deref(), Some(TESTED_TIP));
    for text in filed_texts(&source) {
        assert!(record.contains(&text), "record missing {text:?}: {record}");
    }
    assert!(
        rig.open_job("fix1").is_none(),
        "the fixture returns before fix1 is queued"
    );
    rig.git("main", NEWER_TIP);
    rig.tick_until("fix1 job", |rig| rig.open_job("fix1").is_some())?;
    let job = rig
        .open_job("fix1")
        .ok_or_else(|| VivariumError::Message("no open fix1 job".into()))?;
    for text in filed_texts(&source) {
        assert!(
            job.body.contains(&text),
            "brief missing {text:?}: {}",
            job.body
        );
    }
    assert!(job.body.contains(&format!("Current main tip: {NEWER_TIP}")));
    let (_, record) = rig
        .port
        .open_record(&rig.need)?
        .ok_or_else(|| VivariumError::Message("the record closed early".into()))?;
    let front = record_of("", &record)
        .map_err(|e| VivariumError::Message(format!("bad filed record: {e}")))?
        .front;
    assert_eq!(front.tested.as_deref(), Some(TESTED_TIP));
    Ok(())
}

#[test]
fn a_ready_research_envelope_lands_the_run_and_closes_the_need() -> R {
    let mut rig = rig(
        PumpConfig::default(),
        "+++\nkind = \"research\"\n+++\n",
        "research",
    )?;
    rig.tick_until("research job", |rig| rig.open_job("research").is_some())?;
    let job = rig
        .open_job("research")
        .ok_or_else(|| VivariumError::Message("no research job".into()))?;
    let envelope = "+++\nanswer = \"READY\"\n+++\n\n## Verdict\nPlan it.";
    rig.finish(&job.id, JobStatus::Finished, &agent_receipt(0, envelope))?;
    rig.tick_until(
        "run done",
        |rig| matches!(rig.runs(), Ok(runs) if runs.iter().any(|run| run.state == "done")),
    )?;
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    // The landing tick re-reads the kind after the record closed and raises
    // `flow-kind-changed` once (a tick defect, not the rig's); nothing else.
    let exceptions = rig.exceptions();
    assert!(
        exceptions
            .lines()
            .all(|line| line.contains("flow-kind-changed")),
        "{exceptions}"
    );
    Ok(())
}
